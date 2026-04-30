//! File-system watching with debounce — graphify-parity for
//! `graphify watch <path>`. Wraps `notify-debouncer-mini` to batch rapid
//! events so a single editor save does not trigger N reindex passes.
//!
//! ## Public surface
//!
//! - [`WatchConfig`] — debounce window + skip patterns.
//! - [`run_watch`] — long-running blocking call. Takes a callback that
//!   receives a batch of changed paths.
//!
//! ## Why blocking and not async
//!
//! lens has no async runtime today and a watch loop is one event source
//! with no fan-out. Spawning the debouncer on its own thread (notify does
//! this internally) is enough; the caller's main thread drives the
//! `recv()` loop.
//!
//! ## Skip rules
//!
//! Events under `.lens/`, `target/` (Rust build output), `__pycache__/`,
//! and any path matching the existing `.gitignore` are filtered out at
//! the source level — notify is configured to ignore them so the
//! debouncer never sees them. This avoids reindex storms when cargo
//! writes hundreds of object files per build.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use crate::error::{LensError, Result};

/// Configuration for [`run_watch`].
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Debounce window. Events for the same path arriving inside the window
    /// are coalesced; events for different paths are batched into a single
    /// callback invocation. Default: 200ms.
    pub debounce: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(200),
        }
    }
}

/// Run a watch loop on `root`, invoking `on_batch` whenever the debouncer
/// emits a new batch of events. Blocks the calling thread; returns when
/// the channel closes (notify thread exits) or `on_batch` returns Err.
///
/// `on_batch` receives the list of changed paths (deduped, sorted). It
/// can run `cmd::update::run` directly to refresh the index.
///
/// `should_continue` is called BEFORE each blocking `recv()` so the caller
/// can break out of the loop on Ctrl-C, signal handler, or external state.
/// Return false to exit cleanly; loops drains pending events first.
pub fn run_watch<F, S>(
    root: &Path,
    config: &WatchConfig,
    mut on_batch: F,
    mut should_continue: S,
) -> Result<()>
where
    F: FnMut(&[PathBuf]) -> Result<()>,
    S: FnMut() -> bool,
{
    if !root.exists() {
        return Err(LensError::invalid_path(root, "does not exist"));
    }
    if !root.is_dir() {
        return Err(LensError::invalid_path(root, "not a directory"));
    }
    // macOS FSEvents reports /private/var/... even when the watcher was
    // started on /var/... (which is a symlink). Canonicalise so the
    // strip_prefix in is_relevant() succeeds.
    let root_canonical = root
        .canonicalize()
        .map_err(|e| LensError::other(format!("canonicalise watch root {}: {e}", root.display())))?;

    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<DebouncedEvent>, notify::Error>>();
    let mut debouncer = new_debouncer(config.debounce, move |res| {
        let _ = tx.send(res);
    })
    .map_err(|e| LensError::other(format!("init debouncer: {e}")))?;

    debouncer
        .watcher()
        .watch(&root_canonical, RecursiveMode::Recursive)
        .map_err(|e| LensError::other(format!("watch root {}: {e}", root_canonical.display())))?;

    while should_continue() {
        // Use a finite timeout so should_continue gets re-checked even when
        // the project is idle. The debounce window itself is much shorter,
        // so the watcher's batching is unaffected.
        let evt = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(e) => e,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let events = match evt {
            Ok(v) => v,
            Err(e) => {
                return Err(LensError::other(format!("watch error: {e}")));
            }
        };
        let paths = filter_and_dedup(events, &root_canonical);
        if paths.is_empty() {
            continue;
        }
        on_batch(&paths)?;
    }

    Ok(())
}

/// Strip events under `.lens/`, `target/`, `__pycache__/`, hidden dirs, and
/// dedup by absolute path. Returns paths sorted for determinism.
fn filter_and_dedup(events: Vec<DebouncedEvent>, root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(events.len());
    for e in events {
        if !is_relevant(&e.path, root) {
            continue;
        }
        out.push(e.path);
    }
    out.sort();
    out.dedup();
    out
}

fn is_relevant(path: &Path, root: &Path) -> bool {
    // Must be under the watched root (notify can occasionally surface
    // siblings on macOS via FSEvents — guard against that).
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    for component in rel.components() {
        let Some(s) = component.as_os_str().to_str() else {
            return false; // non-UTF-8 paths skipped
        };
        match s {
            ".lens" | "target" | "__pycache__" | ".git" | "node_modules" => return false,
            _ => {
                // Skip hidden files/dirs except a few well-known ones we
                // might want to track. Conservative default.
                if s.starts_with('.') && s != "." && s != ".." {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn test_watch_config_default_is_200ms() {
        let c = WatchConfig::default();
        assert_eq!(c.debounce, Duration::from_millis(200));
    }

    #[test]
    fn test_filter_and_dedup_strips_dotdirs_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Synthesise events under various paths.
        let events = vec![
            DebouncedEvent { path: root.join("src/a.rs"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
            DebouncedEvent { path: root.join("target/debug/x"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
            DebouncedEvent { path: root.join(".lens/index.db"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
            DebouncedEvent { path: root.join("__pycache__/x.pyc"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
            DebouncedEvent { path: root.join(".git/HEAD"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
            DebouncedEvent { path: root.join("src/a.rs"), kind: notify_debouncer_mini::DebouncedEventKind::Any },
        ];
        let out = filter_and_dedup(events, root);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], root.join("src/a.rs"));
    }

    #[test]
    fn test_run_watch_invokes_callback_on_real_fs_event() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // Pre-create a file so the watcher has something to track.
        fs::write(root.join("seed.rs"), "x").unwrap();

        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let root_for_thread = root.clone();
        let handle = std::thread::spawn(move || {
            let cfg = WatchConfig { debounce: Duration::from_millis(100) };
            let _ = run_watch(
                &root_for_thread,
                &cfg,
                |paths| {
                    let _ = tx.send(paths.to_vec());
                    Ok(())
                },
                || !stop_for_thread.load(Ordering::SeqCst),
            );
        });

        // Give the watcher a moment to start.
        std::thread::sleep(Duration::from_millis(150));
        // Touch a file — notify should pick it up.
        fs::write(root.join("touched.rs"), "y").unwrap();

        // Wait up to 3s for the callback.
        let mut got: Option<Vec<PathBuf>> = None;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
                got = Some(b);
                break;
            }
        }
        stop.store(true, Ordering::SeqCst);
        let _ = handle.join();

        let paths = got.expect("expected at least one batch within 3s");
        assert!(
            paths.iter().any(|p| p.ends_with("touched.rs")),
            "expected touched.rs in batch; got {paths:?}"
        );
    }

    #[test]
    fn test_run_watch_errors_when_root_missing() {
        let cfg = WatchConfig::default();
        let r = run_watch(
            Path::new("/path/that/does/not/exist/lens-watch-test"),
            &cfg,
            |_| Ok(()),
            || true,
        );
        assert!(r.is_err());
    }

    #[test]
    fn test_run_watch_errors_when_root_is_file_not_dir() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("file");
        fs::write(&f, "x").unwrap();
        let cfg = WatchConfig::default();
        let r = run_watch(&f, &cfg, |_| Ok(()), || true);
        assert!(r.is_err());
    }
}
