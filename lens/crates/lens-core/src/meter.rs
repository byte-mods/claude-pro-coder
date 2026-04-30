//! `lens meter` — persistent token meter.
//!
//! Tracks cumulative input/output tokens and call counts across Claude Code
//! turns and `/clear` resets. The meter survives session boundaries because
//! it lives on disk under `.lens/meter.txt`, not in conversation context.
//!
//! Storage format: a tiny `key=value\n` file, one pair per line. Hand-rolled
//! to avoid pulling serde + serde_json into the dependency tree just for a
//! six-integer counter.
//!
//! Recording is opt-in: callers (e.g. the super-coder skill, or a wrapper
//! script) call `lens meter --record-input N --record-output M` after a
//! Claude turn to bump the counters. Without recordings, every read returns
//! zero.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{LensError, Result};

const METER_FILENAME: &str = "meter.txt";

/// Cumulative counters since the last `--reset`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeterCounters {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub calls: u64,
}

/// On-disk meter state. Holds current counters plus a snapshot taken at the
/// last `lens meter` (non-recording) invocation, used for `--diff`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeterState {
    pub current: MeterCounters,
    pub last_invoked: MeterCounters,
    /// Unix-epoch seconds of the most recent `record` call. Zero if never recorded.
    pub last_updated_unix: u64,
    /// Unix-epoch seconds of the most recent non-recording read. Zero if never read.
    pub last_invoked_unix: u64,
}

impl MeterState {
    /// Compute the diff between `current` and `last_invoked`. Negative diffs
    /// (i.e. counters that went down between snapshots — e.g. due to a
    /// `--reset`) saturate at zero so the report stays interpretable.
    pub fn diff(&self) -> MeterCounters {
        MeterCounters {
            input_tokens: self.current.input_tokens.saturating_sub(self.last_invoked.input_tokens),
            output_tokens: self.current.output_tokens.saturating_sub(self.last_invoked.output_tokens),
            calls: self.current.calls.saturating_sub(self.last_invoked.calls),
        }
    }
}

/// Path to the meter file given a `.lens/` directory.
pub fn meter_path(lens_dir: &Path) -> PathBuf {
    lens_dir.join(METER_FILENAME)
}

/// Read the meter file at `lens_dir/meter.txt`. Returns a default state when
/// the file does not exist (first-run case). Malformed lines are silently
/// ignored — meter is observability, not durable state, so a corrupt file
/// shouldn't poison subsequent runs.
pub fn read_state(lens_dir: &Path) -> Result<MeterState> {
    let path = meter_path(lens_dir);
    if !path.exists() {
        return Ok(MeterState::default());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| LensError::other(format!("meter: read {}: {e}", path.display())))?;
    Ok(parse_state(&raw))
}

/// Parse a meter-file body into [`MeterState`]. Pure — no fs.
pub fn parse_state(s: &str) -> MeterState {
    let mut state = MeterState::default();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v: u64 = match v.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        match k.trim() {
            "input_tokens" => state.current.input_tokens = v,
            "output_tokens" => state.current.output_tokens = v,
            "calls" => state.current.calls = v,
            "last_invoked_input_tokens" => state.last_invoked.input_tokens = v,
            "last_invoked_output_tokens" => state.last_invoked.output_tokens = v,
            "last_invoked_calls" => state.last_invoked.calls = v,
            "last_updated_unix" => state.last_updated_unix = v,
            "last_invoked_unix" => state.last_invoked_unix = v,
            _ => {}
        }
    }
    state
}

/// Render [`MeterState`] back to the on-disk format. Pure — no fs.
pub fn render_state(s: &MeterState) -> String {
    format!(
        "input_tokens={}\n\
         output_tokens={}\n\
         calls={}\n\
         last_invoked_input_tokens={}\n\
         last_invoked_output_tokens={}\n\
         last_invoked_calls={}\n\
         last_updated_unix={}\n\
         last_invoked_unix={}\n",
        s.current.input_tokens,
        s.current.output_tokens,
        s.current.calls,
        s.last_invoked.input_tokens,
        s.last_invoked.output_tokens,
        s.last_invoked.calls,
        s.last_updated_unix,
        s.last_invoked_unix
    )
}

/// Write the meter state atomically (temp file + rename). Creates `lens_dir`
/// if missing — meter is allowed to operate even before `lens init` has run,
/// so e.g. a wrapper script can record before the project's index exists.
pub fn write_state(lens_dir: &Path, state: &MeterState) -> Result<()> {
    fs::create_dir_all(lens_dir)
        .map_err(|e| LensError::other(format!("meter: create_dir_all {}: {e}", lens_dir.display())))?;
    let path = meter_path(lens_dir);
    let tmp = lens_dir.join(format!(".meter.staging.{}", std::process::id()));
    fs::write(&tmp, render_state(state))
        .map_err(|e| LensError::other(format!("meter: write tmp {}: {e}", tmp.display())))?;
    fs::rename(&tmp, &path)
        .map_err(|e| LensError::other(format!("meter: rename to {}: {e}", path.display())))?;
    Ok(())
}

/// Increment counters and update `last_updated_unix`. Called when the user
/// runs `lens meter --record-input X --record-output Y`.
pub fn record(state: &mut MeterState, input: u64, output: u64) {
    state.current.input_tokens = state.current.input_tokens.saturating_add(input);
    state.current.output_tokens = state.current.output_tokens.saturating_add(output);
    state.current.calls = state.current.calls.saturating_add(1);
    state.last_updated_unix = now_unix();
}

/// Snapshot `current` into `last_invoked` and bump `last_invoked_unix`.
/// Called when the user runs a non-recording `lens meter` invocation —
/// gives `--diff` something to compare against next time.
pub fn snapshot_invocation(state: &mut MeterState) {
    state.last_invoked = state.current;
    state.last_invoked_unix = now_unix();
}

/// Zero the current counters but preserve `last_invoked` so the next `--diff`
/// reports the negative delta (saturating, see [`MeterState::diff`]). Reset
/// also clears `last_updated_unix`.
pub fn reset(state: &mut MeterState) {
    state.current = MeterCounters::default();
    state.last_updated_unix = 0;
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
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_meter_default_state_is_all_zeros() {
        let s = MeterState::default();
        assert_eq!(s.current, MeterCounters::default());
        assert_eq!(s.last_invoked, MeterCounters::default());
        assert_eq!(s.last_updated_unix, 0);
    }

    #[test]
    fn test_meter_round_trip_through_disk() {
        let d = tmp();
        let mut s = MeterState::default();
        record(&mut s, 1500, 800);
        record(&mut s, 200, 100);
        write_state(d.path(), &s).unwrap();
        let r = read_state(d.path()).unwrap();
        assert_eq!(r.current.input_tokens, 1700);
        assert_eq!(r.current.output_tokens, 900);
        assert_eq!(r.current.calls, 2);
        assert!(r.last_updated_unix > 0);
    }

    #[test]
    fn test_meter_read_returns_default_for_missing_file() {
        let d = tmp();
        let s = read_state(d.path()).unwrap();
        assert_eq!(s, MeterState::default());
    }

    #[test]
    fn test_meter_parse_ignores_blank_and_comment_lines() {
        let body = "\n\
            # this is a comment\n\
            input_tokens=42\n\
            \n\
            output_tokens=10\n\
            # another comment\n\
            calls=3\n";
        let s = parse_state(body);
        assert_eq!(s.current.input_tokens, 42);
        assert_eq!(s.current.output_tokens, 10);
        assert_eq!(s.current.calls, 3);
    }

    #[test]
    fn test_meter_parse_ignores_malformed_lines_without_panicking() {
        let body = "not_a_pair\n\
            input_tokens=notanumber\n\
            =42\n\
            output_tokens=99\n";
        let s = parse_state(body);
        assert_eq!(s.current.input_tokens, 0, "malformed value rejected");
        assert_eq!(s.current.output_tokens, 99, "well-formed line still accepted");
    }

    #[test]
    fn test_meter_record_increments_counters_and_call_count() {
        let mut s = MeterState::default();
        record(&mut s, 1000, 500);
        assert_eq!(s.current, MeterCounters { input_tokens: 1000, output_tokens: 500, calls: 1 });
        record(&mut s, 250, 125);
        assert_eq!(s.current, MeterCounters { input_tokens: 1250, output_tokens: 625, calls: 2 });
    }

    #[test]
    fn test_meter_reset_zeros_current_but_keeps_last_invoked() {
        let mut s = MeterState::default();
        record(&mut s, 100, 50);
        snapshot_invocation(&mut s);
        record(&mut s, 200, 100);
        reset(&mut s);
        assert_eq!(s.current, MeterCounters::default(), "current zeroed");
        assert_eq!(s.last_invoked, MeterCounters { input_tokens: 100, output_tokens: 50, calls: 1 });
        assert_eq!(s.last_updated_unix, 0, "last_updated cleared on reset");
    }

    #[test]
    fn test_meter_snapshot_copies_current_into_last_invoked() {
        let mut s = MeterState::default();
        record(&mut s, 100, 50);
        snapshot_invocation(&mut s);
        assert_eq!(s.last_invoked, s.current);
        assert!(s.last_invoked_unix > 0);
    }

    #[test]
    fn test_meter_diff_returns_delta_between_snapshots() {
        let mut s = MeterState::default();
        record(&mut s, 100, 50);
        snapshot_invocation(&mut s);
        record(&mut s, 250, 125);
        let d = s.diff();
        assert_eq!(d, MeterCounters { input_tokens: 250, output_tokens: 125, calls: 1 });
    }

    #[test]
    fn test_meter_diff_saturates_at_zero_when_current_below_snapshot() {
        let mut s = MeterState::default();
        record(&mut s, 100, 50);
        snapshot_invocation(&mut s);
        reset(&mut s); // current → 0, but last_invoked still has 100/50/1
        let d = s.diff();
        assert_eq!(d, MeterCounters::default(), "diff saturates rather than going negative");
    }

    #[test]
    fn test_meter_render_round_trips_through_parse() {
        let mut s = MeterState::default();
        record(&mut s, 1500, 800);
        snapshot_invocation(&mut s);
        record(&mut s, 200, 100);
        let body = render_state(&s);
        let parsed = parse_state(&body);
        assert_eq!(parsed.current, s.current);
        assert_eq!(parsed.last_invoked, s.last_invoked);
        assert_eq!(parsed.last_updated_unix, s.last_updated_unix);
        assert_eq!(parsed.last_invoked_unix, s.last_invoked_unix);
    }
}
