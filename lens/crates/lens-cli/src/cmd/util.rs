//! Shared helpers used by multiple `cmd::*` modules.

use std::path::{Path, PathBuf};

use lens_core::{ensure_fresh, FreshnessConfig, Storage};

/// Open the project's `.lens/index.db` for reading and run an auto-freshness
/// check (silently). Errors from the freshness check are swallowed so a
/// transient discover/extract failure never blocks a query — Claude can
/// still get back a slightly-stale answer rather than no answer at all.
///
/// Returns `(storage, db_path)` so callers can include the path in error
/// messages without re-deriving it. `Err(message)` is returned for genuine
/// open failures (DB missing, schema corrupt) — those should surface.
pub fn open_with_auto_freshness(root: &Path, label: &str) -> Result<(Storage, PathBuf), String> {
    let db_path: PathBuf = root.join(".lens").join("index.db");
    if !db_path.exists() {
        return Err(format!(
            "lens {label}: '{}' does not exist. Run `lens index` first.",
            db_path.display()
        ));
    }
    let mut storage = match Storage::open(&db_path) {
        Ok(s) => s,
        Err(e) => return Err(format!("lens {label}: failed to open index: {e}")),
    };

    // Best-effort freshness check. Errors here only mean the index might be
    // stale relative to disk — the read still works. We deliberately do not
    // print to stdout so command output stays clean for piping; stderr is
    // used only for hard errors.
    let cfg = FreshnessConfig::from_env();
    let _ = ensure_fresh(&mut storage, root, cfg);

    Ok((storage, db_path))
}
