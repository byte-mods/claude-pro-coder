//! `lens add` — fetch a URL into `.lens/raw/`. graphify-parity for
//! `graphify add <url>`.
//!
//! When the saved file's extension matches a registered language, the
//! file is also fed into the index (CASCADE-replace semantics via
//! `update_files`). Otherwise it is just stored.

use std::path::Path;

use lens_core::storage::{insert::unix_seconds_now, update_files, Storage};
use lens_core::walk::DiscoveredFile;
use lens_core::{fetch_to_raw, run_pipeline_on_discovered, FetchResult, LanguageId, Registry};

pub fn run(url: &str) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens add: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, url)
}

pub fn run_with_root(root: &Path, url: &str) -> Result<(), u8> {
    let lens_dir = root.join(".lens");
    if let Err(e) = std::fs::create_dir_all(&lens_dir) {
        eprintln!("lens add: cannot create '{}': {e}", lens_dir.display());
        return Err(1);
    }

    let fetched = match fetch_to_raw(url, &lens_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lens add: fetch failed: {e}");
            return Err(1);
        }
    };
    let already = if fetched.already_present { " (dedup)" } else { "" };
    println!(
        "lens add: saved {} bytes to {}{}",
        fetched.bytes,
        fetched.saved_to.display(),
        already,
    );

    // If the extension matches a registered language, index the file
    // through the regular pipeline. Otherwise it's just a saved blob.
    let registry = Registry::with_default_languages();
    let Some(lang) = lang_from_path(&fetched.saved_to, &registry) else {
        println!("lens add: extension not registered for code extraction; saved-only.");
        return Ok(());
    };

    if fetched.already_present {
        // Already indexed during a prior add — nothing more to do.
        return Ok(());
    }

    let db_path = lens_dir.join("index.db");
    if !db_path.exists() {
        // No prior index — caller should run `lens index` to bootstrap.
        eprintln!(
            "lens add: file is indexable but '{}' does not exist. Run `lens index` first to bootstrap, then re-run `lens add`.",
            db_path.display()
        );
        return Err(1);
    }

    let mut storage = match Storage::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens add: failed to open index: {e}");
            return Err(1);
        }
    };

    let discovered = match build_discovered(&fetched, root, lang) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lens add: cannot build discovery record: {e}");
            return Err(1);
        }
    };
    let extracted = match run_pipeline_on_discovered(&[discovered], &registry) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("lens add: extraction failed: {e}");
            return Err(1);
        }
    };
    let upd = match update_files(&mut storage, &extracted, &[]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens add: insert failed: {e}");
            return Err(1);
        }
    };
    let resolve = match lens_core::resolve_cross_file_references(&mut storage) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens add: resolve failed: {e}");
            return Err(1);
        }
    };

    println!(
        "lens add: indexed {} symbols / {} refs / {} calls / {} imports / {} type-rels (resolved: {} refs / {} calls / {} types / {} imports)",
        upd.symbols, upd.refs, upd.calls, upd.imports, upd.type_relations,
        resolve.resolved_refs, resolve.resolved_calls, resolve.resolved_types, resolve.resolved_imports,
    );
    Ok(())
}

fn lang_from_path(path: &Path, registry: &Registry) -> Option<LanguageId> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    registry.language_for_extension(&ext)
}

fn build_discovered(
    fetched: &FetchResult,
    root: &Path,
    lang: LanguageId,
) -> std::result::Result<DiscoveredFile, String> {
    // Project-relative path: the saved blob is at `.lens/raw/<host>/<sha8>.<ext>`,
    // which is unconditionally under `root`. Compute the relative form.
    let rel = fetched
        .saved_to
        .strip_prefix(root)
        .map_err(|e| format!("strip_prefix: {e}"))?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(DiscoveredFile {
        relative_path: rel,
        absolute_path: fetched.saved_to.clone(),
        language: lang,
        content_hash: fetched.content_hash,
        size_bytes: fetched.bytes,
        modified_at: unix_seconds_now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    #[test]
    fn test_add_run_saves_text_blob_when_no_index() {
        // Non-source-code extension just lands in raw/, no error.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("payload.txt");
        write(&src, "just a text payload\n");

        let url = format!("file://{}", src.display());
        let r = run_with_root(root, &url);
        assert_eq!(r, Ok(()));
        // Saved path should exist somewhere under .lens/raw/.
        let raw_dir = root.join(".lens").join("raw");
        assert!(raw_dir.exists());
    }

    #[test]
    fn test_add_run_indexes_python_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Bootstrap empty index.
        crate::cmd::index::run(Some(root)).expect("initial index");

        // Local fixture: a Python file with an extension the registry
        // recognises.
        let src = root.join("fixture.py");
        write(&src, "def added():\n    pass\n");
        let url = format!("file://{}", src.display());

        let r = run_with_root(root, &url);
        assert_eq!(r, Ok(()));

        // Verify the symbol is in the DB.
        let storage = lens_core::Storage::open(root.join(".lens/index.db")).unwrap();
        let n: i64 = storage
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE name = 'added'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "expected the python symbol 'added' in the index");
    }

    #[test]
    fn test_add_run_dedup_on_second_call_with_same_url() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        crate::cmd::index::run(Some(root)).expect("initial index");

        let src = root.join("dup.py");
        write(&src, "def x():\n    pass\n");
        let url = format!("file://{}", src.display());

        run_with_root(root, &url).unwrap();
        // Second call must not error, must not double-insert symbols.
        run_with_root(root, &url).unwrap();
        let storage = lens_core::Storage::open(root.join(".lens/index.db")).unwrap();
        let n: i64 = storage
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE name = 'x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "second add must dedup, not double-insert");
    }

    #[test]
    fn test_add_run_indexable_without_initial_index_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("orphan.py");
        write(&src, "def y():\n    pass\n");
        let url = format!("file://{}", src.display());

        let r = run_with_root(root, &url);
        assert_eq!(r, Err(1));
        // The file should still have been saved.
        let raw_dir = root.join(".lens").join("raw");
        assert!(raw_dir.exists());
    }

    #[test]
    fn test_add_run_invalid_url_errors() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "not a url");
        assert_eq!(r, Err(1));
    }
}
