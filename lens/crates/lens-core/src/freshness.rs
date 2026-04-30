//! Auto-freshness — keep the lens index in sync with the working tree
//! without requiring the user to remember `lens update`.
//!
//! Strategy: every read-mode command (follow / refs / query / explain / path /
//! slice / map) opens with [`ensure_fresh`]. The function:
//!   1. Reads `.lens/freshness.txt` for the timestamp of the last check.
//!   2. If less than [`THROTTLE_SECONDS`] have elapsed, skips the check
//!      entirely. This makes back-to-back lens calls effectively free.
//!   3. Otherwise walks the project, diffs against the index, and runs an
//!      incremental update when anything drifted.
//!   4. Stamps the timestamp file regardless of outcome (so the throttle
//!      window starts again).
//!
//! Failure mode: if anything goes wrong (walker errors, IO errors,
//! tree-sitter failures), the function returns the error to the caller but
//! does NOT abort the read — callers wrap with `let _ = ensure_fresh(...);`
//! so a stale check never blocks a query that the user would otherwise get
//! a slightly-stale answer to.
//!
//! Opt-out:
//!   - Set env var `LENS_NO_AUTO_UPDATE=1` to disable auto-freshness for
//!     a session.
//!   - The throttle window is configurable via [`Config::throttle_seconds`].

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{LensError, Result};
use crate::extract::run_on_discovered;
use crate::lang::Registry;
use crate::storage::{diff_against_index, resolve_cross_file_references, update_files, Storage, UpdateStats};
use crate::walk::discover;

/// Default minimum gap between two freshness checks. Each read within this
/// window after a check skips the work entirely.
pub const THROTTLE_SECONDS: u64 = 5;

const FRESHNESS_FILE: &str = "freshness.txt";

/// Tunable freshness behaviour. The default is "auto-update enabled with a
/// 5-second throttle". Disable via [`Config::disabled`] to skip entirely.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub disabled: bool,
    pub throttle_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            disabled: false,
            throttle_seconds: THROTTLE_SECONDS,
        }
    }
}

impl Config {
    /// Resolve from env: `LENS_NO_AUTO_UPDATE=1` flips `disabled` true.
    /// `LENS_FRESHNESS_THROTTLE_SECONDS=N` overrides the throttle window.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if matches!(std::env::var("LENS_NO_AUTO_UPDATE").as_deref(), Ok("1") | Ok("true")) {
            cfg.disabled = true;
        }
        if let Ok(s) = std::env::var("LENS_FRESHNESS_THROTTLE_SECONDS") {
            if let Ok(n) = s.parse::<u64>() {
                cfg.throttle_seconds = n;
            }
        }
        cfg
    }
}

/// What `ensure_fresh` did. Useful for callers (and tests) that want to
/// distinguish "throttled" from "actually ran a check" from "ran a check and
/// re-extracted N files".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessOutcome {
    /// Auto-update was disabled (config or env).
    Disabled,
    /// Within the throttle window — skipped without checking.
    Throttled,
    /// Checked the tree, found no drift.
    UpToDate,
    /// Checked the tree, ran an incremental update. Counts mirror
    /// [`UpdateStats`] but flatten to bare integers for lightweight return.
    Updated { changed: u64, new: u64, deleted: u64 },
}

/// Run the freshness check for the project rooted at `root`. Idempotent;
/// safe to call from any read-mode command. Errors propagate but the caller
/// is encouraged to swallow them so a freshness hiccup never blocks a query.
pub fn ensure_fresh(
    storage: &mut Storage,
    root: &Path,
    config: Config,
) -> Result<FreshnessOutcome> {
    if config.disabled {
        return Ok(FreshnessOutcome::Disabled);
    }
    let lens_dir = root.join(".lens");
    let freshness_path = lens_dir.join(FRESHNESS_FILE);

    if let Some(last) = read_timestamp(&freshness_path) {
        if elapsed_since(last) < config.throttle_seconds {
            return Ok(FreshnessOutcome::Throttled);
        }
    }

    // Always stamp the timestamp before doing real work — that way a panic
    // or error halfway through doesn't trap us in a tight retry loop.
    write_timestamp(&freshness_path, now_unix())?;

    let registry = Registry::with_default_languages();
    let discovered = discover(root, &registry)
        .map_err(|e| LensError::other(format!("freshness: discover: {e}")))?;
    let diff = diff_against_index(storage, &discovered)
        .map_err(|e| LensError::other(format!("freshness: diff: {e}")))?;
    if diff.is_empty() {
        return Ok(FreshnessOutcome::UpToDate);
    }

    // Re-extract changed + new files via the parallel pipeline.
    let to_extract: Vec<_> = diff
        .changed
        .iter()
        .chain(diff.new.iter())
        .cloned()
        .collect();
    let extracted = run_on_discovered(&to_extract, &registry)
        .map_err(|e| LensError::other(format!("freshness: extract: {e}")))?;
    // update_files takes `&[&str]` for delete-paths; build that view from
    // diff.deleted (Vec<String>) without copying the strings.
    let delete_refs: Vec<&str> = diff.deleted.iter().map(|s| s.as_str()).collect();
    let stats: UpdateStats = update_files(storage, &extracted, &delete_refs)
        .map_err(|e| LensError::other(format!("freshness: update: {e}")))?;
    resolve_cross_file_references(storage)
        .map_err(|e| LensError::other(format!("freshness: resolve: {e}")))?;

    Ok(FreshnessOutcome::Updated {
        changed: stats.files_replaced,
        new: stats.files_added,
        deleted: stats.files_deleted,
    })
}

fn read_timestamp(path: &Path) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse().ok()
}

fn write_timestamp(path: &Path, ts: u64) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| LensError::other(format!("freshness: create_dir_all: {e}")))?;
    }
    let tmp: PathBuf = match path.parent() {
        Some(p) => p.join(format!(".freshness.staging.{}", std::process::id())),
        None => return Err(LensError::other("freshness: path has no parent".to_string())),
    };
    fs::write(&tmp, ts.to_string())
        .map_err(|e| LensError::other(format!("freshness: write tmp: {e}")))?;
    fs::rename(&tmp, path)
        .map_err(|e| LensError::other(format!("freshness: rename: {e}")))?;
    Ok(())
}

fn elapsed_since(unix_ts: u64) -> u64 {
    now_unix().saturating_sub(unix_ts)
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
    use crate::extract::ExtractedFile;
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use std::fs as std_fs;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        let storage = Storage::open(&path).unwrap();
        (dir, storage)
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std_fs::create_dir_all(p).unwrap();
        }
        std_fs::write(path, body).unwrap();
    }

    #[test]
    fn test_ensure_fresh_returns_disabled_when_config_disabled() {
        let (dir, mut s) = tmp_storage();
        let cfg = Config { disabled: true, throttle_seconds: 0 };
        let outcome = ensure_fresh(&mut s, dir.path(), cfg).unwrap();
        assert_eq!(outcome, FreshnessOutcome::Disabled);
    }

    #[test]
    fn test_ensure_fresh_throttled_within_window() {
        let dir = tempfile::tempdir().unwrap();
        let lens_dir = dir.path().join(".lens");
        std_fs::create_dir_all(&lens_dir).unwrap();
        let path = lens_dir.join(FRESHNESS_FILE);
        std_fs::write(&path, now_unix().to_string()).unwrap();

        let storage_path = dir.path().join(".lens").join("index.db");
        let mut s = Storage::open(&storage_path).unwrap();
        let cfg = Config { disabled: false, throttle_seconds: 60 };
        let outcome = ensure_fresh(&mut s, dir.path(), cfg).unwrap();
        assert_eq!(outcome, FreshnessOutcome::Throttled);
    }

    #[test]
    fn test_ensure_fresh_uptodate_when_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        let storage_path = dir.path().join(".lens").join("index.db");
        std_fs::create_dir_all(storage_path.parent().unwrap()).unwrap();
        let mut s = Storage::open(&storage_path).unwrap();
        // No source files, no index entries → diff empty → UpToDate.
        let cfg = Config { disabled: false, throttle_seconds: 0 };
        let outcome = ensure_fresh(&mut s, dir.path(), cfg).unwrap();
        assert_eq!(outcome, FreshnessOutcome::UpToDate);
    }

    #[test]
    fn test_ensure_fresh_detects_new_file_and_runs_update() {
        let dir = tempfile::tempdir().unwrap();
        let storage_path = dir.path().join(".lens").join("index.db");
        std_fs::create_dir_all(storage_path.parent().unwrap()).unwrap();
        let mut s = Storage::open(&storage_path).unwrap();

        // Drop a Rust source file in the project.
        write_file(&dir.path().join("a.rs"), "pub fn hello() {}\n");

        let cfg = Config { disabled: false, throttle_seconds: 0 };
        let outcome = ensure_fresh(&mut s, dir.path(), cfg).unwrap();
        match outcome {
            FreshnessOutcome::Updated { new, .. } => {
                assert!(new >= 1, "expected at least one new file; got {new}");
            }
            other => panic!("expected Updated; got {other:?}"),
        }

        // Subsequent call within throttle window short-circuits.
        let outcome2 = ensure_fresh(
            &mut s,
            dir.path(),
            Config { disabled: false, throttle_seconds: 60 },
        )
        .unwrap();
        assert_eq!(outcome2, FreshnessOutcome::Throttled);
    }

    #[test]
    fn test_ensure_fresh_detects_modified_file_via_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        let storage_path = dir.path().join(".lens").join("index.db");
        std_fs::create_dir_all(storage_path.parent().unwrap()).unwrap();
        let mut s = Storage::open(&storage_path).unwrap();

        // Pre-populate the index with one file's metadata.
        let mut ef = ExtractedFile::empty("a.rs", LanguageId::Rust);
        ef.content_hash = [0u8; 32]; // Stale hash that won't match disk.
        ef.size_bytes = 0;
        ef.modified_at = 0;
        insert_extracted_files(&mut s, &[ef]).unwrap();

        // Now write a real file at that path with different content.
        write_file(&dir.path().join("a.rs"), "pub fn newer() {}\n");

        let outcome = ensure_fresh(&mut s, dir.path(), Config { disabled: false, throttle_seconds: 0 }).unwrap();
        match outcome {
            FreshnessOutcome::Updated { changed, .. } => {
                assert!(changed >= 1, "expected at least one changed file; got {changed}");
            }
            other => panic!("expected Updated; got {other:?}"),
        }
    }

    #[test]
    fn test_config_from_env_picks_up_disable_var() {
        // Save and restore around the env mutation.
        let prev = std::env::var("LENS_NO_AUTO_UPDATE").ok();
        std::env::set_var("LENS_NO_AUTO_UPDATE", "1");
        let cfg = Config::from_env();
        assert!(cfg.disabled);
        match prev {
            Some(v) => std::env::set_var("LENS_NO_AUTO_UPDATE", v),
            None => std::env::remove_var("LENS_NO_AUTO_UPDATE"),
        }
    }

    #[test]
    fn test_config_from_env_picks_up_throttle_override() {
        let prev = std::env::var("LENS_FRESHNESS_THROTTLE_SECONDS").ok();
        std::env::set_var("LENS_FRESHNESS_THROTTLE_SECONDS", "30");
        let cfg = Config::from_env();
        assert_eq!(cfg.throttle_seconds, 30);
        match prev {
            Some(v) => std::env::set_var("LENS_FRESHNESS_THROTTLE_SECONDS", v),
            None => std::env::remove_var("LENS_FRESHNESS_THROTTLE_SECONDS"),
        }
    }

    #[test]
    fn test_ensure_fresh_stamps_timestamp_after_run() {
        let dir = tempfile::tempdir().unwrap();
        let storage_path = dir.path().join(".lens").join("index.db");
        std_fs::create_dir_all(storage_path.parent().unwrap()).unwrap();
        let mut s = Storage::open(&storage_path).unwrap();
        ensure_fresh(&mut s, dir.path(), Config { disabled: false, throttle_seconds: 0 }).unwrap();
        let stamp = dir.path().join(".lens").join(FRESHNESS_FILE);
        assert!(stamp.exists(), "freshness timestamp file must exist after a run");
    }
}
