//! Composite-tree integration test — pipeline + insert + resolve over a
//! mixed Rust/Python project. Pinned in Section 3 as the largest coverage
//! gap from Section 2 part 2 (per-module unit tests proved each file was
//! handled in isolation; this test proves they compose correctly).
//!
//! Project layout:
//!   src/lib.rs        — Rust crate root (lib.rs collapses to "src" prefix)
//!   src/main.rs       — Rust binary root (main.rs also collapses to "src")
//!   src/foo/mod.rs    — Rust module (mod.rs collapses to "src::foo")
//!   src/foo/bar.rs    — Rust submodule under src/foo
//!   pkg/__init__.py   — Python package init (collapses to "pkg")
//!   pkg/sub.py        — Python submodule under pkg
//!
//! Assertions:
//!   1. Every code file is discovered and indexed.
//!   2. Each language produces qnames in its own convention (`::` for Rust,
//!      `.` for Python) and they do NOT cross-pollute.
//!   3. Symbols in the special "collapsing" files (lib.rs, main.rs, mod.rs,
//!      __init__.py) get the parent-dir's module_path, not their basename's.
//!   4. resolve_cross_file_references runs without error over the composite
//!      symbol set (mixed languages, mixed conventions).

use std::fs;
use std::path::Path;

use lens_core::storage::{
    insert_extracted_files, resolve_cross_file_references, Storage,
};
use lens_core::{run_pipeline, Registry};

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn test_composite_rust_python_project_resolves_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Rust side.
    write(
        &root.join("src/lib.rs"),
        "pub fn lib_root() -> u32 { 0 }\n",
    );
    write(
        &root.join("src/main.rs"),
        "fn main_entry() {}\n",
    );
    write(
        &root.join("src/foo/mod.rs"),
        "pub fn foo_mod() {}\n",
    );
    write(
        &root.join("src/foo/bar.rs"),
        "pub fn bar_fn() {}\n",
    );

    // Python side.
    write(
        &root.join("pkg/__init__.py"),
        "def pkg_init():\n    return 1\n",
    );
    write(
        &root.join("pkg/sub.py"),
        "def sub_fn():\n    return 2\n",
    );

    let registry = Registry::with_default_languages();
    let files = run_pipeline(root, &registry).expect("pipeline");
    assert_eq!(files.len(), 6, "expected 6 indexed files, got {}", files.len());

    // Verify each file's symbols carry the expected qname per convention.
    let by_path: std::collections::HashMap<&str, &lens_core::ExtractedFile> =
        files.iter().map(|f| (f.relative_path.as_str(), f)).collect();

    let assert_first_qname = |rel: &str, expected_qname: &str| {
        let f = by_path
            .get(rel)
            .unwrap_or_else(|| panic!("file {rel} not indexed"));
        let q: Vec<&str> = f.symbols.iter().map(|s| s.qualified_name.as_str()).collect();
        assert!(
            q.contains(&expected_qname),
            "file {rel} expected to contain qname {expected_qname:?}; got {q:?}"
        );
    };

    // Rust convention: `::` separators, lib/main/mod collapse to parent dir.
    assert_first_qname("src/lib.rs", "src::lib_root");
    assert_first_qname("src/main.rs", "src::main_entry");
    assert_first_qname("src/foo/mod.rs", "src::foo::foo_mod");
    assert_first_qname("src/foo/bar.rs", "src::foo::bar::bar_fn");

    // Python convention: `.` separators, __init__.py collapses to parent dir.
    assert_first_qname("pkg/__init__.py", "pkg.pkg_init");
    assert_first_qname("pkg/sub.py", "pkg.sub.sub_fn");

    // Cross-pollution check: no Rust qname uses `.`, no Python qname uses `::`.
    for f in &files {
        for s in &f.symbols {
            let q = &s.qualified_name;
            match f.language {
                lens_core::LanguageId::Rust => {
                    assert!(
                        !q.contains('.') || q.is_empty(),
                        "Rust symbol qname must not contain '.' separator: {q}"
                    );
                }
                lens_core::LanguageId::Python => {
                    assert!(
                        !q.contains("::"),
                        "Python symbol qname must not contain '::' separator: {q}"
                    );
                }
                lens_core::LanguageId::TypeScript
                | lens_core::LanguageId::JavaScript
                | lens_core::LanguageId::Go
                | lens_core::LanguageId::Dart => {
                    // TypeScript / JavaScript / Go each have dedicated unit
                    // tests covering qname-shape contracts. Composite test
                    // does not exercise these languages directly.
                }
            }
        }
    }

    // End-to-end: insert and resolve must succeed over the composite set.
    let db_path = root.join(".lens").join("index.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let mut storage = Storage::open(&db_path).expect("open");
    let ins = insert_extracted_files(&mut storage, &files).expect("insert");
    assert_eq!(ins.files, 6);
    assert!(
        ins.symbols >= 6,
        "expected >=6 symbols across all files, got {}",
        ins.symbols
    );

    let res = resolve_cross_file_references(&mut storage).expect("resolve");
    // No cross-file references in this minimal corpus; resolve must succeed
    // without panic and report zero or more legitimate matches.
    let _ = res;

    // Sanity: confirm the symbols table contains both `::` and `.` qnames
    // and nothing got mangled at insert time.
    let conn = storage.connection();
    let n_rust: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE qualified_name LIKE 'src::%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let n_py: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE qualified_name LIKE 'pkg.%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n_rust >= 4, "expected >=4 src:: qnames, got {n_rust}");
    assert!(n_py >= 2, "expected >=2 pkg. qnames, got {n_py}");
}

#[test]
fn test_composite_pipeline_distinct_module_paths_per_language() {
    // Variant: confirm two files in different languages but with identical
    // structural location (e.g. project-root single file) do not collide on
    // qname. Rust `helper` at root → "helper"; Python `helper` at root →
    // "helper" (bare). They DO share name & qname when both are bare — that's
    // a documented MINOR but not a defect; this test pins the contract so a
    // future refactor that "fixes" the collision is caught.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a.rs"), "pub fn helper() {}\n");
    write(&root.join("a.py"), "def helper():\n    pass\n");

    let registry = Registry::with_default_languages();
    let files = run_pipeline(root, &registry).expect("pipeline");
    assert_eq!(files.len(), 2);

    let qnames: Vec<&str> = files
        .iter()
        .flat_map(|f| f.symbols.iter().map(|s| s.qualified_name.as_str()))
        .collect();
    // Both files produce qname "a::helper" / "a.helper" — different convention,
    // so they DO differ. This pins the convention-distinctness contract.
    assert!(qnames.contains(&"a::helper"), "missing Rust a::helper: {qnames:?}");
    assert!(qnames.contains(&"a.helper"), "missing Python a.helper: {qnames:?}");
}
