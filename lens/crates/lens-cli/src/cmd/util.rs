//! Shared helpers used by multiple `cmd::*` modules.

use std::path::{Path, PathBuf};

use lens_core::{
    ensure_fresh, estimate_tokens, record_lens_on_disk, total_file_tokens, FreshnessConfig, Storage,
};

/// Locate the project root for a read verb invoked from anywhere inside the
/// project: the nearest ancestor of `start` (inclusive) that holds
/// `.lens/index.db`, else the nearest ancestor holding a `.git` entry, else
/// `start` itself. Mirrors how an IDE resolves "the project" from an open
/// file so `lens follow` works from a sub-directory just as it does from
/// the root.
pub fn find_project_root(start: &Path) -> PathBuf {
    let mut git_root: Option<PathBuf> = None;
    for dir in start.ancestors() {
        if dir.join(".lens").join("index.db").is_file() {
            return dir.to_path_buf();
        }
        if git_root.is_none() && dir.join(".git").exists() {
            git_root = Some(dir.to_path_buf());
        }
    }
    git_root.unwrap_or_else(|| start.to_path_buf())
}

/// Current directory resolved through [`find_project_root`]. The common
/// prologue of every read verb's `run()`.
pub fn cwd_project_root(label: &str) -> Result<PathBuf, u8> {
    match std::env::current_dir() {
        Ok(p) => Ok(find_project_root(&p)),
        Err(e) => {
            eprintln!("lens {label}: cannot resolve current directory: {e}");
            Err(1)
        }
    }
}

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

/// Append the token footer to a rendered read-verb result and record the
/// call in the persistent meter (`.lens/meter.txt`).
///
/// `touched_files` are the project-relative files the model would have had
/// to read whole to get the same information; the footer reports the
/// difference as tokens saved. Pass an empty slice when there is no
/// meaningful counterfactual (e.g. `lens map`).
///
/// The footer itself is counted in the emitted total so the meter never
/// under-reports. Meter writes are best-effort: a read-only `.lens/` must
/// not turn a successful query into a failure.
pub fn finalize(root: &Path, storage: &Storage, mut rendered: String, touched_files: &[&str]) -> String {
    let body_tokens = estimate_tokens(&rendered) as u64;
    let full = total_file_tokens(storage, root, touched_files.iter().copied());
    let saved = full.saturating_sub(body_tokens);
    let footer = if touched_files.is_empty() {
        format!("_tokens: ~{body_tokens} emitted_\n")
    } else {
        let n = {
            let mut v: Vec<&str> = touched_files.to_vec();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        format!(
            "_tokens: ~{body_tokens} emitted • ~{saved} saved vs reading {n} file{} whole (~{full})_\n",
            if n == 1 { "" } else { "s" }
        )
    };
    let emitted = body_tokens + estimate_tokens(&footer) as u64;
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str(&footer);
    let _ = record_lens_on_disk(&root.join(".lens"), emitted, saved);
    rendered
}

/// `finalize` + print to stdout. The common CLI tail.
pub fn finish(root: &Path, storage: &Storage, rendered: String, touched_files: &[&str]) {
    print!("{}", finalize(root, storage, rendered, touched_files));
}

/// Backstop for any future stub subcommand. Currently unused (all subcommands
/// now have real implementations); retained so adding a stub is cheap.
#[allow(dead_code)]
pub fn not_yet(subcommand: &str, section: u32) -> Result<(), u8> {
    eprintln!(
        "lens {subcommand}: not yet implemented (section {section}). \
         Run `lens --help` for status of available subcommands."
    );
    Err(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::read_meter;
    use std::fs;

    #[test]
    fn test_finalize_appends_footer_and_records_meter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("big.rs"), "fn a() {}\n".repeat(200)).unwrap();
        let storage = Storage::open(root.join(".lens").join("index.db")).unwrap();
        let out = finalize(root, &storage, "# Follow: `a`\n".to_string(), &["big.rs"]);
        assert!(out.contains("_tokens: ~"), "{out}");
        assert!(out.contains("saved vs reading 1 file whole"), "{out}");
        let m = read_meter(&root.join(".lens")).unwrap();
        assert_eq!(m.current.lens_calls, 1);
        assert!(m.current.lens_emitted_tokens > 0);
        assert!(m.current.lens_saved_tokens > 0, "200-line file must beat a one-line card");
    }

    #[test]
    fn test_finalize_without_touched_files_reports_emitted_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let storage = Storage::open(root.join(".lens").join("index.db")).unwrap();
        let out = finalize(root, &storage, "# Map\n".to_string(), &[]);
        assert!(out.ends_with("emitted_\n"), "{out}");
        assert!(!out.contains("saved"));
        let m = read_meter(&root.join(".lens")).unwrap();
        assert_eq!(m.current.lens_saved_tokens, 0);
    }

    #[test]
    fn test_finalize_dedupes_touched_files_in_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("x.rs"), "fn x() {}\n").unwrap();
        let storage = Storage::open(root.join(".lens").join("index.db")).unwrap();
        let out = finalize(root, &storage, "x".to_string(), &["x.rs", "x.rs"]);
        assert!(out.contains("reading 1 file whole"), "{out}");
    }
    #[test]
    fn test_find_project_root_walks_up_to_lens_index_then_git_then_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let deep = root.join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        // Nothing found → start itself.
        assert_eq!(find_project_root(&deep), deep);
        // A .git marker above wins over nothing.
        fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(find_project_root(&deep), root);
        // A .lens index in between wins over .git.
        fs::create_dir_all(root.join("a").join(".lens")).unwrap();
        fs::write(root.join("a").join(".lens").join("index.db"), b"").unwrap();
        assert_eq!(find_project_root(&deep), root.join("a"));
    }
}
