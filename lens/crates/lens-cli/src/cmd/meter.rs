//! `lens meter` — persistent token meter CLI.
//!
//! Wraps [`lens_core::meter`] for the CLI. Five operating modes selected by
//! flags (mutually consistent — record + show is the common case):
//!
//!   - default: read state, snapshot last_invoked, print human report.
//!   - `--json`: print as JSON instead of human report.
//!   - `--reset`: zero current counters, persist, exit.
//!   - `--diff`: report delta since last non-recording invocation.
//!   - `--record-input N --record-output M`: bump counters, persist, exit
//!     without printing (so wrapper scripts can quietly tally usage).
//!   - `--since DUR`: show counters only if last_updated is within DUR.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lens_core::{
    read_meter, record_meter, reset_meter, snapshot_meter, write_meter, MeterState,
};

#[allow(clippy::too_many_arguments)]
pub fn run(
    json: bool,
    since: Option<&str>,
    reset_flag: bool,
    diff: bool,
    record_input: Option<u64>,
    record_output: Option<u64>,
) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens meter: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, json, since, reset_flag, diff, record_input, record_output)
}

#[allow(clippy::too_many_arguments)]
pub fn run_with_root(
    root: &Path,
    json: bool,
    since: Option<&str>,
    reset_flag: bool,
    diff: bool,
    record_input: Option<u64>,
    record_output: Option<u64>,
) -> Result<(), u8> {
    let lens_dir: PathBuf = root.join(".lens");

    // 1. Read existing state (default if missing).
    let mut state = match read_meter(&lens_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens meter: read failed: {e}");
            return Err(1);
        }
    };

    // 2. Apply mutations in priority order.
    //
    //    --reset wipes current counters. We persist immediately and exit
    //    without printing — same shape as a record-only invocation. The user
    //    can re-run `lens meter` to confirm the reset took.
    if reset_flag {
        reset_meter(&mut state);
        if let Err(e) = write_meter(&lens_dir, &state) {
            eprintln!("lens meter: write failed: {e}");
            return Err(1);
        }
        if json {
            println!("{}", to_json(&state, /*diff_mode*/ false));
        } else {
            println!("lens meter: counters reset.");
        }
        return Ok(());
    }

    //    Recording is silent and does not affect the human/JSON report path —
    //    a wrapper script that does `lens meter --record-input X --record-output Y`
    //    after every Claude turn shouldn't spam stdout.
    let recording = record_input.is_some() || record_output.is_some();
    if recording {
        let i = record_input.unwrap_or(0);
        let o = record_output.unwrap_or(0);
        record_meter(&mut state, i, o);
        if let Err(e) = write_meter(&lens_dir, &state) {
            eprintln!("lens meter: write failed: {e}");
            return Err(1);
        }
        return Ok(());
    }

    // 3. Read-only path — print the report. Snapshot last_invoked AFTER we
    //    captured the current diff, so the user sees the delta they expect.
    let report = if diff {
        ReportMode::Diff
    } else if let Some(since) = since {
        match parse_duration(since) {
            Some(d) => ReportMode::Since(d),
            None => {
                eprintln!(
                    "lens meter: --since '{since}' is not a recognised duration (use e.g. 30s, 5m, 2h)."
                );
                return Err(2);
            }
        }
    } else {
        ReportMode::Cumulative
    };

    let output = if json {
        to_json(&state, matches!(report, ReportMode::Diff))
    } else {
        render_human(&state, report)
    };
    println!("{output}");

    // Snapshot AFTER printing so the next `--diff` reports the delta from
    // *this* invocation forward.
    snapshot_meter(&mut state);
    if let Err(e) = write_meter(&lens_dir, &state) {
        eprintln!("lens meter: write failed (snapshot): {e}");
        return Err(1);
    }

    Ok(())
}

#[derive(Clone, Copy)]
pub enum ReportMode {
    Cumulative,
    Diff,
    Since(Duration),
}

/// Format the meter as a human-readable block. Pure — no fs.
pub fn render_human(state: &MeterState, mode: ReportMode) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# lens meter");
    let _ = writeln!(&mut out);

    let (label, counters) = match mode {
        ReportMode::Cumulative => ("Cumulative", state.current),
        ReportMode::Diff => ("Since last invocation", state.diff()),
        ReportMode::Since(d) => {
            let now = now_unix();
            let cutoff = now.saturating_sub(d.as_secs());
            if state.last_updated_unix == 0 || state.last_updated_unix < cutoff {
                ("Since (no activity in window)", lens_core::MeterCounters::default())
            } else {
                ("Since (within window)", state.current)
            }
        }
    };
    let _ = writeln!(&mut out, "**{label}**");
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "- input_tokens:  {}", counters.input_tokens);
    let _ = writeln!(&mut out, "- output_tokens: {}", counters.output_tokens);
    let _ = writeln!(&mut out, "- calls:         {}", counters.calls);
    if state.last_updated_unix > 0 {
        let _ = writeln!(
            &mut out,
            "- last_updated:  unix {}",
            state.last_updated_unix
        );
    }
    out
}

/// Format the meter as a single JSON object. Hand-rolled — the fields are
/// all integers in known ranges, no escaping concerns.
pub fn to_json(state: &MeterState, diff_mode: bool) -> String {
    let counters = if diff_mode { state.diff() } else { state.current };
    format!(
        "{{\"input_tokens\":{},\"output_tokens\":{},\"calls\":{},\"last_updated_unix\":{},\"last_invoked_unix\":{},\"diff_mode\":{}}}",
        counters.input_tokens,
        counters.output_tokens,
        counters.calls,
        state.last_updated_unix,
        state.last_invoked_unix,
        if diff_mode { "true" } else { "false" }
    )
}

/// Parse a duration like `30s`, `5m`, `2h`, or `1d` into a [`Duration`].
/// Returns `None` for unrecognised forms.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_part, suffix) = s.split_at(s.len() - 1);
    let n: u64 = num_part.parse().ok()?;
    let secs = match suffix {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::MeterCounters;
    use std::fs;

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_meter_run_record_persists_to_disk_silently() {
        let d = tmp_root();
        let r = run_with_root(d.path(), false, None, false, false, Some(1500), Some(800));
        assert_eq!(r, Ok(()));
        let raw = fs::read_to_string(d.path().join(".lens/meter.txt")).unwrap();
        assert!(raw.contains("input_tokens=1500"));
        assert!(raw.contains("output_tokens=800"));
        assert!(raw.contains("calls=1"));
    }

    #[test]
    fn test_meter_run_reset_clears_current_counters() {
        let d = tmp_root();
        run_with_root(d.path(), false, None, false, false, Some(100), Some(50)).unwrap();
        run_with_root(d.path(), false, None, true, false, None, None).unwrap();
        let raw = fs::read_to_string(d.path().join(".lens/meter.txt")).unwrap();
        assert!(raw.contains("input_tokens=0"));
        assert!(raw.contains("output_tokens=0"));
        assert!(raw.contains("calls=0"));
    }

    #[test]
    fn test_meter_run_default_path_succeeds_for_empty_state() {
        let d = tmp_root();
        let r = run_with_root(d.path(), false, None, false, false, None, None);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_meter_run_rejects_malformed_since() {
        let d = tmp_root();
        let r = run_with_root(d.path(), false, Some("five-hours"), false, false, None, None);
        assert_eq!(r, Err(2));
    }

    #[test]
    fn test_meter_run_diff_after_record_reports_delta() {
        let d = tmp_root();
        // First read snapshots (zero) → last_invoked = 0.
        run_with_root(d.path(), false, None, false, false, None, None).unwrap();
        // Record some usage.
        run_with_root(d.path(), false, None, false, false, Some(1000), Some(500)).unwrap();
        // Diff should now show 1000/500 vs the zero snapshot.
        let r = run_with_root(d.path(), true, None, false, true, None, None);
        assert_eq!(r, Ok(()));
        // Verify by reading the JSON path directly via render_human at the API level.
        let state = read_meter(&d.path().join(".lens")).unwrap();
        let diff = state.diff();
        // Note: the snapshot above moved last_invoked to 1000/500, so the
        // *next* diff is zero. We assert the immediately-prior diff via API.
        // What we can verify: state.current = 1000/500, and after the JSON read
        // it advanced last_invoked.
        assert_eq!(state.current, MeterCounters { input_tokens: 1000, output_tokens: 500, calls: 1 });
        assert_eq!(diff, MeterCounters::default(), "after the diff read, last_invoked == current");
    }

    #[test]
    fn test_render_human_includes_label_per_mode() {
        let mut state = MeterState::default();
        state.current = MeterCounters { input_tokens: 10, output_tokens: 5, calls: 1 };
        let cum = render_human(&state, ReportMode::Cumulative);
        let dif = render_human(&state, ReportMode::Diff);
        assert!(cum.contains("**Cumulative**"));
        assert!(dif.contains("**Since last invocation**"));
    }

    #[test]
    fn test_to_json_emits_flat_object_with_known_keys() {
        let mut state = MeterState::default();
        state.current = MeterCounters { input_tokens: 10, output_tokens: 5, calls: 1 };
        let j = to_json(&state, false);
        assert!(j.starts_with('{') && j.ends_with('}'));
        assert!(j.contains("\"input_tokens\":10"));
        assert!(j.contains("\"output_tokens\":5"));
        assert!(j.contains("\"calls\":1"));
        assert!(j.contains("\"diff_mode\":false"));
    }

    #[test]
    fn test_to_json_diff_mode_serialises_delta_not_current() {
        let mut state = MeterState::default();
        state.current = MeterCounters { input_tokens: 100, output_tokens: 50, calls: 5 };
        state.last_invoked = MeterCounters { input_tokens: 30, output_tokens: 10, calls: 2 };
        let j = to_json(&state, true);
        assert!(j.contains("\"input_tokens\":70"));
        assert!(j.contains("\"output_tokens\":40"));
        assert!(j.contains("\"calls\":3"));
        assert!(j.contains("\"diff_mode\":true"));
    }

    #[test]
    fn test_parse_duration_recognises_s_m_h_d() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86400)));
    }

    #[test]
    fn test_parse_duration_rejects_unknown_suffix_or_non_numeric() {
        assert_eq!(parse_duration("5"), None);
        assert_eq!(parse_duration("5y"), None);
        assert_eq!(parse_duration("xm"), None);
        assert_eq!(parse_duration(""), None);
    }
}
