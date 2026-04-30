//! `lens update` — incremental rebuild. Walks the project, diffs against the
//! persisted index, re-extracts only the changed/new subset, removes stale
//! entries, and re-runs cross-file resolution.
//!
//! Replaces the section-3 contract that `lens index` errors on its second
//! run. The flow:
//!
//!   1. resolve project root, ensure `.lens/index.db` exists (error otherwise).
//!   2. walk → `Vec<DiscoveredFile>`
//!   3. diff against the index → `FileDiff { unchanged, changed, new, deleted }`
//!   4. extract only the `changed + new` subset via `run_on_discovered`
//!   5. `update_files(storage, extracted, deleted)` — atomic CASCADE-replace
//!   6. `resolve_cross_file_references(storage)` to refresh cross-file FKs
//!   7. print summary
//!
//! Compared to a full re-index (`lens index`), this skips parse + extract on
//! files whose blake3 hash hasn't changed. For projects with thousands of
//! source files and a small change set, this is the difference between
//! seconds and tens of seconds.
//!
//! ## Re-resolution scope
//!
//! `resolve_cross_file_references` runs over the WHOLE database, not just
//! the changed subset, because a renamed top-level symbol can affect
//! references in untouched files that referred to it by qname. This is a
//! full-table scan but the resolver's UPDATEs are gated by `WHERE FK IS
//! NULL` — already-resolved rows are no-ops.

use std::path::{Path, PathBuf};

use lens_core::storage::{
    diff_against_index, resolve_cross_file_references, update_files, FileDiff, ResolveStats,
    Storage, UpdateStats,
};
use lens_core::{discover, run_pipeline_on_discovered, Registry};

/// Run `lens update` against `path` (defaults to current working directory).
///
/// Errors with a non-zero exit code if:
/// - `path` is missing or not a directory,
/// - `.lens/index.db` does not exist (user must run `lens init` + `lens index`
///   first — `lens update` is incremental, not "create from scratch"),
/// - any of walk / diff / extract / update / resolve fails.
pub fn run(path: Option<&Path>) -> Result<(), u8> {
    let project_root: PathBuf = match path {
        Some(p) => p.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                eprintln!("lens update: cannot resolve current directory: {e}");
                return Err(1);
            }
        },
    };
    if !project_root.exists() {
        eprintln!("lens update: '{}' does not exist", project_root.display());
        return Err(1);
    }
    if !project_root.is_dir() {
        eprintln!("lens update: '{}' is not a directory", project_root.display());
        return Err(1);
    }

    let lens_dir = project_root.join(".lens");
    let db_path = lens_dir.join("index.db");
    if !db_path.exists() {
        eprintln!(
            "lens update: '{}' does not exist. Run `lens index` first to build the initial index.",
            db_path.display()
        );
        return Err(1);
    }

    let mut storage = match Storage::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens update: failed to open index database: {e}");
            return Err(1);
        }
    };

    let registry = Registry::with_default_languages();

    let discovered = match discover(&project_root, &registry) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lens update: discovery failed: {e}");
            return Err(1);
        }
    };

    let diff: FileDiff = match diff_against_index(&storage, &discovered) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lens update: diff failed: {e}");
            return Err(1);
        }
    };

    if diff.is_empty() {
        println!(
            "lens update: no changes detected ({} unchanged file{}, nothing to do).",
            diff.unchanged.len(),
            if diff.unchanged.len() == 1 { "" } else { "s" }
        );
        return Ok(());
    }

    // Combine changed + new for the extract pass — both need the per-file
    // parse + extract. Deleted paths are not extracted (just removed from
    // the index).
    let to_extract: Vec<lens_core::DiscoveredFile> = diff
        .changed
        .iter()
        .cloned()
        .chain(diff.new.iter().cloned())
        .collect();

    let extracted = match run_pipeline_on_discovered(&to_extract, &registry) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lens update: extraction failed: {e}");
            return Err(1);
        }
    };

    let deleted_paths: Vec<&str> = diff.deleted.iter().map(String::as_str).collect();
    let update_stats = match update_files(&mut storage, &extracted, &deleted_paths) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens update: update_files failed: {e}");
            return Err(1);
        }
    };

    let resolve_stats = match resolve_cross_file_references(&mut storage) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens update: resolve failed: {e}");
            return Err(1);
        }
    };

    print_summary(&db_path, &diff, &update_stats, &resolve_stats);
    Ok(())
}

fn print_summary(
    db_path: &Path,
    diff: &FileDiff,
    upd: &UpdateStats,
    res: &ResolveStats,
) {
    println!(
        "lens update: {} changed / {} new / {} deleted / {} unchanged → {}",
        diff.changed.len(),
        diff.new.len(),
        diff.deleted.len(),
        diff.unchanged.len(),
        db_path.display(),
    );
    println!(
        "lens update: re-extracted {} symbols / {} refs / {} calls / {} imports / {} type-rels",
        upd.symbols, upd.refs, upd.calls, upd.imports, upd.type_relations,
    );
    println!(
        "lens update: re-resolved {} refs / {} calls / {} types / {} imports across files",
        res.resolved_refs, res.resolved_calls, res.resolved_types, res.resolved_imports,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn build_initial_index(root: &Path) {
        // Bootstrap: run `lens index` machinery (without going through the
        // CLI) so `update` has something to compare against.
        crate::cmd::index::run(Some(root)).expect("initial index");
    }

    #[test]
    fn test_update_run_errors_when_lens_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // No `.lens/` created — update must refuse rather than silently
        // bootstrap.
        let r = run(Some(root));
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_update_run_no_changes_is_clean_noop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn a() {}\n");
        build_initial_index(root);

        // Second invocation against unchanged source must succeed cleanly.
        let r = run(Some(root));
        assert_eq!(r, Ok(()));

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let n_files: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_files, 1);
    }

    #[test]
    fn test_update_run_picks_up_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn original() {}\n");
        build_initial_index(root);

        // Modify the file — the new symbol name must replace the old one.
        write(&root.join("src/a.rs"), "pub fn renamed() {}\n");
        let r = run(Some(root));
        assert_eq!(r, Ok(()));

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let conn = storage.connection();
        let names: Vec<String> = conn
            .prepare("SELECT name FROM symbols ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["renamed"], "old symbol must have been CASCADE-deleted");
    }

    #[test]
    fn test_update_run_picks_up_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn a() {}\n");
        build_initial_index(root);

        write(&root.join("src/b.rs"), "pub fn b() {}\n");
        run(Some(root)).expect("update");

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let n_files: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_files, 2);
    }

    #[test]
    fn test_update_run_removes_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn a() {}\n");
        write(&root.join("src/b.rs"), "pub fn b() {}\n");
        build_initial_index(root);

        fs::remove_file(root.join("src/b.rs")).unwrap();
        run(Some(root)).expect("update");

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let conn = storage.connection();
        let paths: Vec<String> = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["src/a.rs"]);
    }

    #[test]
    fn test_update_run_handles_mixed_change_new_delete() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/keep.rs"), "pub fn keep() {}\n");
        write(&root.join("src/touch.rs"), "pub fn original() {}\n");
        write(&root.join("src/gone.rs"), "pub fn gone() {}\n");
        build_initial_index(root);

        write(&root.join("src/touch.rs"), "pub fn touched() {}\n");
        write(&root.join("src/fresh.rs"), "pub fn fresh() {}\n");
        fs::remove_file(root.join("src/gone.rs")).unwrap();

        run(Some(root)).expect("update");

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let conn = storage.connection();
        let paths: Vec<String> = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["src/fresh.rs", "src/keep.rs", "src/touch.rs"]);

        let names: Vec<String> = conn
            .prepare("SELECT name FROM symbols ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["fresh", "keep", "touched"]);
    }

    #[test]
    fn test_update_run_rejects_nonexistent_path() {
        let bogus = PathBuf::from("/this/path/definitely/does/not/exist/lens-update-test");
        let r = run(Some(&bogus));
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_update_run_rejects_non_directory_path() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("notadir.txt");
        fs::write(&f, "x").unwrap();
        let r = run(Some(&f));
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_update_run_re_resolves_cross_file_references() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("util.rs"), "pub fn helper() -> u32 { 1 }\n");
        write(
            &root.join("main.rs"),
            "use util::helper;\nfn main() { let _ = helper(); }\n",
        );
        build_initial_index(root);

        // Touch util.rs with a new symbol; the import in main.rs must
        // re-resolve to the new symbol's rowid.
        write(
            &root.join("util.rs"),
            "pub fn helper() -> u32 { 2 }\npub fn extra() {}\n",
        );
        run(Some(root)).expect("update");

        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let resolved: i64 = storage
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM imports WHERE resolved_symbol_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved, 1, "the import must still be resolved post-update");
    }
}
