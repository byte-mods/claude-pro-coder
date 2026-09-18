use std::path::{Path, PathBuf};

use lens_core::storage::{
    rebuild_extracted_files, resolve_cross_file_references, InsertStats, ResolveStats, Storage,
};
use lens_core::{run_pipeline, sync_docs, DocsStats, Registry};

/// Build (or rebuild) the lens index for `path` (defaults to the current
/// working directory). Writes to `.lens/index.db` under the project root.
///
/// Pipeline:
///   1. resolve project root
///   2. ensure `.lens/` exists, open `.lens/index.db` (creates + migrates)
///   3. discover + parse + extract via [`run_pipeline`]
///   4. atomic rebuild of the code index ([`rebuild_extracted_files`]) —
///      idempotent: re-running replaces the previous index in one transaction
///   5. cross-file FK resolution ([`resolve_cross_file_references`])
///   6. any-language text index sync ([`sync_docs`]) for `lens search`
///   7. print summary
pub fn run(path: Option<&Path>) -> Result<(), u8> {
    let project_root: PathBuf = match path {
        Some(p) => p.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                eprintln!("lens index: cannot resolve current directory: {e}");
                return Err(1);
            }
        },
    };
    if !project_root.exists() {
        eprintln!("lens index: '{}' does not exist", project_root.display());
        return Err(1);
    }
    if !project_root.is_dir() {
        eprintln!("lens index: '{}' is not a directory", project_root.display());
        return Err(1);
    }

    let lens_dir = project_root.join(".lens");
    if let Err(e) = std::fs::create_dir_all(&lens_dir) {
        eprintln!("lens index: cannot create '{}': {e}", lens_dir.display());
        return Err(1);
    }
    let db_path = lens_dir.join("index.db");

    let mut storage = match Storage::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens index: failed to open index database: {e}");
            return Err(1);
        }
    };

    let registry = Registry::with_default_languages();
    let files = match run_pipeline(&project_root, &registry) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lens index: extraction failed: {e}");
            return Err(1);
        }
    };

    let insert_stats = match rebuild_extracted_files(&mut storage, &files) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens index: insert failed: {e}");
            return Err(1);
        }
    };

    let resolve_stats = match resolve_cross_file_references(&mut storage) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens index: resolve failed: {e}");
            return Err(1);
        }
    };

    let docs_stats = match sync_docs(&mut storage, &project_root, &registry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens index: text index failed: {e}");
            return Err(1);
        }
    };

    print_summary(&db_path, &insert_stats, &resolve_stats, &docs_stats);
    Ok(())
}

fn print_summary(db_path: &Path, ins: &InsertStats, res: &ResolveStats, docs: &DocsStats) {
    println!(
        "lens index: wrote {} files / {} symbols / {} refs / {} calls / {} imports / {} type-rels to {}",
        ins.files,
        ins.symbols,
        ins.refs,
        ins.calls,
        ins.imports,
        ins.type_relations,
        db_path.display(),
    );
    println!(
        "lens index: resolved {} refs / {} calls / {} types / {} imports across files",
        res.resolved_refs, res.resolved_calls, res.resolved_types, res.resolved_imports,
    );
    println!(
        "lens index: text index {} files ({} skipped as binary/oversized) — `lens search` ready",
        docs.total_indexed(),
        docs.skipped,
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

    #[test]
    fn test_index_run_writes_to_lens_db() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/lib.rs"), "pub fn hello() -> u32 { 42 }\n");

        run(Some(root)).expect("index run");
        let db = root.join(".lens").join("index.db");
        assert!(db.exists(), "expected .lens/index.db to be written");

        // Sanity: at least one file row was inserted.
        let storage = Storage::open(&db).expect("re-open");
        let n: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 1, "expected at least one indexed file, got {n}");
    }

    #[test]
    fn test_index_run_summary_counts_correct() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Project layout chosen so the qname produced by the Rust extractor
        // matches the import's raw_path. With files at project root:
        //   util.rs    → module_path "util",  helper qname "util::helper"
        //   main.rs    → module_path ""       (main.rs collapses to parent dir)
        //   `use util::helper;` in main.rs → raw_path "util::helper"
        // qnames and raw_path align, so cross-file resolution succeeds.
        write(&root.join("util.rs"), "pub fn helper() -> u32 { 1 }\n");
        write(
            &root.join("main.rs"),
            "use util::helper;\n\
             fn main() { let _ = helper(); }\n",
        );

        run(Some(root)).expect("index run");

        let db = root.join(".lens").join("index.db");
        let storage = Storage::open(&db).expect("re-open");
        let conn = storage.connection();

        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 2);

        // helper symbol must exist with the expected qname.
        let helper_qname: String = conn
            .query_row(
                "SELECT qualified_name FROM symbols WHERE name = 'helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(helper_qname, "util::helper");

        // The import row in main.rs should resolve back to the helper symbol.
        let resolved_imports: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM imports WHERE resolved_symbol_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            resolved_imports, 1,
            "expected exactly one resolved import (use util::helper)"
        );

        // The cross-file call (main.rs calls bare `helper`) is linked through
        // the import-aware heuristic phase: `use util::helper` resolved to
        // the helper symbol, and the bare callee name matches it.
        let resolved_calls: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM calls c JOIN symbols s ON s.id = c.callee_symbol_id
                 WHERE s.qualified_name = 'util::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved_calls, 1, "bare-name cross-file call must resolve via the import");
    }

    #[test]
    fn test_index_run_empty_project_succeeds_with_zero_stats() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // No code files at all — pipeline returns Vec::new(), inserts zero,
        // resolves zero. Must succeed and write a fresh DB.
        run(Some(root)).expect("index run on empty project");

        let db = root.join(".lens").join("index.db");
        assert!(db.exists());
        let storage = Storage::open(&db).expect("re-open");
        let n: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_index_run_is_idempotent_and_replaces_previous_index() {
        // Re-running `lens index` must replace the index atomically rather
        // than fail on the `files.path` UNIQUE constraint (the v1 contract).
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn a() {}\n");

        run(Some(root)).expect("first index run");
        write(&root.join("src/a.rs"), "pub fn a_renamed() {}\n");
        run(Some(root)).expect("second index run must succeed");

        let db = root.join(".lens").join("index.db");
        let storage = Storage::open(&db).expect("re-open");
        let files: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "no duplicate file rows after a rebuild");
        let names: Vec<String> = storage
            .connection()
            .prepare("SELECT name FROM symbols")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["a_renamed"]);
        let docs: i64 = storage
            .connection()
            .query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(docs, 1, "text index populated by lens index");
    }

    #[test]
    fn test_index_run_rejects_nonexistent_path() {
        let bogus = PathBuf::from("/this/path/definitely/does/not/exist");
        let res = run(Some(&bogus));
        assert!(res.is_err());
    }

    #[test]
    fn test_index_run_rejects_non_directory_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("notadir.txt");
        write(&file, "not a directory");
        let res = run(Some(&file));
        assert!(res.is_err());
    }
}
