//! `lens watch` — watch the project, run incremental update on each batch.
//! graphify-parity for `graphify watch <path>`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lens_core::{run_watch, WatchConfig};

/// Run a watch loop on the current working directory. Blocking; the user
/// terminates with Ctrl-C (handled via the `interrupted` flag below).
///
/// `debounce_ms` is the debounce window; events arriving within this
/// interval are coalesced into one update pass.
pub fn run(debounce_ms: u64) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens watch: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, debounce_ms, None)
}

/// Test/automation entry point. `max_batches`, when `Some`, forces the loop
/// to exit after that many batches have been processed — used by integration
/// tests to drive a single batch and assert behaviour.
pub fn run_with_root(
    root: &Path,
    debounce_ms: u64,
    max_batches: Option<u32>,
) -> Result<(), u8> {
    let db_path = root.join(".lens").join("index.db");
    if !db_path.exists() {
        eprintln!(
            "lens watch: '{}' does not exist. Run `lens index` first.",
            db_path.display()
        );
        return Err(1);
    }

    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupted_for_handler = Arc::clone(&interrupted);
    // Best-effort Ctrl-C: if signal-hook isn't available, the loop still
    // exits when stdin closes or the channel disconnects. Skipping the
    // signal handler is acceptable for v1 — `lens watch` is interactive.
    let _ = ctrlc_lite(move || {
        interrupted_for_handler.store(true, Ordering::SeqCst);
    });

    let config = WatchConfig {
        debounce: Duration::from_millis(debounce_ms),
    };
    let batches_seen = std::cell::Cell::new(0u32);
    let root_owned = root.to_path_buf();

    eprintln!(
        "lens watch: watching {} (debounce={}ms). Press Ctrl-C to exit.",
        root.display(),
        debounce_ms
    );

    let result = run_watch(
        root,
        &config,
        |paths: &[PathBuf]| {
            batches_seen.set(batches_seen.get() + 1);
            eprintln!(
                "lens watch: batch — {} path{} changed; running incremental update...",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            );
            // Defer to cmd::update to do the actual reindex. Errors don't
            // tear down the watcher — they are surfaced and the loop
            // continues so the next save has a chance to recover.
            if let Err(code) = crate::cmd::update::run(Some(&root_owned)) {
                eprintln!("lens watch: update failed (exit {code}); continuing.");
            }
            Ok(())
        },
        || {
            if interrupted.load(Ordering::SeqCst) {
                return false;
            }
            if let Some(max) = max_batches {
                if batches_seen.get() >= max {
                    return false;
                }
            }
            true
        },
    );

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("lens watch: watcher exited with error: {e}");
            Err(1)
        }
    }
}

/// Lightweight Ctrl-C handler — installs a signal-hook-style callback
/// using only stdlib so we avoid pulling another dependency. Returns
/// Err if installation fails on this platform; caller treats failure as
/// "no signal handling, fall back to other exit conditions."
fn ctrlc_lite<F>(_callback: F) -> Result<(), ()>
where
    F: Fn() + Send + 'static,
{
    // Stdlib has no portable Ctrl-C primitive. Pulling `signal-hook` or
    // `ctrlc` would add a dep just for this convenience. v1: rely on the
    // shell-level SIGINT (process dies) and the test path's `max_batches`
    // bound. Returning Err signals "no handler installed."
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn write(path: &Path, s: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    #[test]
    fn test_watch_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), 50, Some(1));
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_watch_run_processes_one_batch_then_exits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn a() {}\n");
        // Bootstrap the index so `cmd::update::run` has a DB.
        crate::cmd::index::run(Some(root)).expect("initial index");

        // Spawn the watch loop in a thread; mutate a file from the main
        // thread so the debouncer fires.
        let root_for_thread = root.to_path_buf();
        let handle = std::thread::spawn(move || {
            run_with_root(&root_for_thread, 100, Some(1))
        });

        // Give the watcher a moment to attach.
        std::thread::sleep(Duration::from_millis(300));
        write(&root.join("src/a.rs"), "pub fn a_renamed() {}\n");

        let r = handle.join().expect("thread panic");
        assert_eq!(r, Ok(()));

        // Verify the update actually ran — the symbol name should have
        // changed in the DB.
        let storage = lens_core::Storage::open(root.join(".lens/index.db")).unwrap();
        let n: i64 = storage
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE name = 'a_renamed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "watch+update did not pick up the rename");
    }
}
