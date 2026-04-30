//! Cross-file reference resolution — second pass after [`crate::storage::insert`].
//!
//! [`insert_extracted_files`] writes each [`crate::extract::ExtractedFile`] with
//! same-file foreign keys resolved (parent_symbol_id, calls.caller_symbol_id,
//! types.symbol_id) and cross-file FKs left NULL. This module fills those NULLs
//! by matching `raw_name` columns against `symbols.qualified_name`.
//!
//! ## Resolution semantics
//!
//! Two-phase per FK column. Phase 1 — global qualified-name match. For each
//! NULL FK row, look up `raw_name` in the global symbols table and fill the
//! FK on hit. Phase 2 — same-file bare-name fallback. For rows still NULL
//! after phase 1, look up `raw_name` against `symbols.name` restricted to the
//! same `file_id` as the ref/call/type-relation. On no match in either phase,
//! leave the FK NULL — unresolved is a valid terminal state (the schema's
//! `ON DELETE` rules tolerate it). Cross-file bare names are resolved through
//! `imports` (see T3).
//!
//! ## Duplicate qnames
//!
//! `qualified_name` is unique per file (`idx_symbols_qname_file`) but NOT
//! globally. Two files may both define a top-level `foo`. When the same qname
//! appears in multiple files (rare in well-organised projects but legal in
//! Python with `from x import *` patterns), this pass picks the symbol with the
//! lowest `id` (insertion order ⇒ filesystem walk order). T2 will refine for
//! intra-file scope priority.
//!
//! ## Concurrency
//!
//! Resolve runs in a single transaction. SQLite + WAL handles concurrent
//! readers; lens does not need concurrent writers in v1. Each of the three
//! UPDATE statements is one SQL roundtrip — no per-row loop in the hot path.

use rusqlite::params;

use crate::error::{LensError, Result};
use crate::storage::Storage;

/// Per-table NULL→FK fill counts for [`resolve_cross_file_references`]. Useful
/// for tests and CLI summary output.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResolveStats {
    pub resolved_refs: u64,
    pub resolved_calls: u64,
    pub resolved_types: u64,
    /// Number of imports rows that gained a non-NULL `resolved_symbol_id`.
    /// `resolved_file_id` is always set in lockstep with `resolved_symbol_id`
    /// in the qname-match path; standalone file-path resolution (for module
    /// imports that aren't bound to a top-level symbol) is deferred.
    pub resolved_imports: u64,
}

/// Run the cross-file resolution passes (qname-first, bare-name same-file
/// fallback) over an already-populated storage. Idempotent — re-running on
/// already-resolved data finds zero rows (the WHERE `IS NULL` clauses skip
/// them).
///
/// Errors abort the transaction; partial fills never persist.
pub fn resolve_cross_file_references(storage: &mut Storage) -> Result<ResolveStats> {
    let tx = storage.transaction()?;

    // Phase 1 — qname-based global match.
    let mut resolved_refs = tx
        .execute(
            "UPDATE refs SET symbol_id = (
                 SELECT id FROM symbols
                 WHERE qualified_name = refs.raw_name
                 ORDER BY id ASC LIMIT 1
             )
             WHERE symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols WHERE qualified_name = refs.raw_name
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve refs by qname: {e}")))?
        as u64;

    let mut resolved_calls = tx
        .execute(
            "UPDATE calls SET callee_symbol_id = (
                 SELECT id FROM symbols
                 WHERE qualified_name = calls.callee_raw_name
                 ORDER BY id ASC LIMIT 1
             )
             WHERE callee_symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols WHERE qualified_name = calls.callee_raw_name
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve calls by qname: {e}")))?
        as u64;

    let mut resolved_types = tx
        .execute(
            "UPDATE types SET target_symbol_id = (
                 SELECT id FROM symbols
                 WHERE qualified_name = types.target_raw_name
                 ORDER BY id ASC LIMIT 1
             )
             WHERE target_symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols WHERE qualified_name = types.target_raw_name
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve types by qname: {e}")))?
        as u64;

    // Phase 2 — bare-name fallback restricted to the row's own file. Only
    // touches rows still NULL after phase 1 (qname always wins).
    resolved_refs += tx
        .execute(
            "UPDATE refs SET symbol_id = (
                 SELECT id FROM symbols
                 WHERE name = refs.raw_name AND file_id = refs.file_id
                 ORDER BY id ASC LIMIT 1
             )
             WHERE symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols
                 WHERE name = refs.raw_name AND file_id = refs.file_id
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve refs by bare name: {e}")))?
        as u64;

    resolved_calls += tx
        .execute(
            "UPDATE calls SET callee_symbol_id = (
                 SELECT id FROM symbols
                 WHERE name = calls.callee_raw_name AND file_id = calls.file_id
                 ORDER BY id ASC LIMIT 1
             )
             WHERE callee_symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols
                 WHERE name = calls.callee_raw_name AND file_id = calls.file_id
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve calls by bare name: {e}")))?
        as u64;

    resolved_types += tx
        .execute(
            "UPDATE types SET target_symbol_id = (
                 SELECT id FROM symbols
                 WHERE name = types.target_raw_name AND file_id = types.file_id
                 ORDER BY id ASC LIMIT 1
             )
             WHERE target_symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols
                 WHERE name = types.target_raw_name AND file_id = types.file_id
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve types by bare name: {e}")))?
        as u64;

    // Phase 3 — imports.raw_path → resolved_symbol_id + resolved_file_id via
    // qname match. The two correlated subqueries pick the same lowest-id row
    // (deterministic), so symbol_id and file_id come from the same symbol.
    // File-path-based resolution for module imports (where raw_path names a
    // module file, not a symbol — e.g. Python `import os`) is deferred:
    // requires language-specific candidate-path generation (`os` →
    // `os.py`/`os/__init__.py`, `crate::a::b` → `src/a/b.rs`/...).
    let resolved_imports = tx
        .execute(
            "UPDATE imports SET
                 resolved_symbol_id = (
                     SELECT id FROM symbols
                     WHERE qualified_name = imports.raw_path
                     ORDER BY id ASC LIMIT 1
                 ),
                 resolved_file_id = (
                     SELECT file_id FROM symbols
                     WHERE qualified_name = imports.raw_path
                     ORDER BY id ASC LIMIT 1
                 )
             WHERE resolved_symbol_id IS NULL
               AND EXISTS (
                 SELECT 1 FROM symbols WHERE qualified_name = imports.raw_path
               )",
            params![],
        )
        .map_err(|e| LensError::other(format!("resolve imports by qname: {e}")))?
        as u64;

    tx.commit()
        .map_err(|e| LensError::other(format!("commit resolve transaction: {e}")))?;

    Ok(ResolveStats {
        resolved_refs,
        resolved_calls,
        resolved_types,
        resolved_imports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{
        ExtractedCall, ExtractedFile, ExtractedRef, ExtractedSymbol, ExtractedTypeRel,
    };
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn make_symbol(qname: &str, name: &str, kind: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: kind.into(),
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 10,
            body_start_byte: 0,
            body_end_byte: 10,
            signature: None,
            visibility: None,
            parent_qualified_name: None,
            doc_comment: None,
        }
    }

    fn make_ref(raw_name: &str, line: u32) -> ExtractedRef {
        ExtractedRef {
            raw_name: raw_name.into(),
            kind: "type_identifier".into(),
            line,
            col: 0,
            end_line: line,
            end_col: 10,
        }
    }

    fn make_call(caller_qname: &str, callee_raw: &str, line: u32) -> ExtractedCall {
        ExtractedCall {
            caller_qualified_name: caller_qname.into(),
            callee_raw_name: callee_raw.into(),
            line,
            col: 0,
        }
    }

    fn make_type(symbol_qname: &str, target_raw: &str, line: u32) -> ExtractedTypeRel {
        ExtractedTypeRel {
            symbol_qualified_name: symbol_qname.into(),
            relation: "implements".into(),
            target_raw_name: target_raw.into(),
            line,
        }
    }

    fn make_import(raw_path: &str, line: u32) -> crate::extract::ExtractedImport {
        crate::extract::ExtractedImport {
            raw_path: raw_path.into(),
            alias: None,
            line,
        }
    }

    fn rust_file(path: &str, symbols: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Rust);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 100;
        ef.modified_at = 1700000000;
        ef.symbols = symbols;
        ef
    }

    #[test]
    fn test_resolve_qname_match_refs() {
        let (_g, mut s) = tmp_storage();
        // File A defines `crate::foo`. File B references `crate::foo`.
        let mut a = rust_file("src/a.rs", vec![make_symbol("crate::foo", "foo", "function")]);
        // refs are owned by file B, not file A — give B a stub caller and the ref.
        a.symbols.push(make_symbol("crate::a_marker", "a_marker", "const"));
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::b_caller", "b_caller", "function")]);
        b.refs = vec![make_ref("crate::foo", 5)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 1);

        let (sym_id, raw_name): (Option<i64>, String) = s
            .connection()
            .query_row(
                "SELECT symbol_id, raw_name FROM refs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let foo_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sym_id, Some(foo_id), "ref must point to the foo symbol id");
        assert_eq!(raw_name, "crate::foo");
    }

    #[test]
    fn test_resolve_qname_match_calls() {
        let (_g, mut s) = tmp_storage();
        let a = rust_file("src/a.rs", vec![make_symbol("crate::callee", "callee", "function")]);
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::caller", "caller", "function")]);
        b.calls = vec![make_call("crate::caller", "crate::callee", 7)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_calls, 1);

        let (callee_id, callee_raw): (Option<i64>, String) = s
            .connection()
            .query_row(
                "SELECT callee_symbol_id, callee_raw_name FROM calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let expected: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::callee'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(callee_id, Some(expected));
        assert_eq!(callee_raw, "crate::callee");
    }

    #[test]
    fn test_resolve_qname_match_types_cross_file() {
        let (_g, mut s) = tmp_storage();
        // File A defines a trait `crate::T`.
        let a = rust_file("src/a.rs", vec![make_symbol("crate::T", "T", "trait")]);
        // File B defines a type `crate::S` that implements `crate::T`.
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::S", "S", "struct")]);
        b.type_relations = vec![make_type("crate::S", "crate::T", 3)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_types, 1);

        let target_id: Option<i64> = s
            .connection()
            .query_row(
                "SELECT target_symbol_id FROM types",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let t_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::T'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target_id, Some(t_id));
    }

    #[test]
    fn test_resolve_no_match_leaves_null() {
        let (_g, mut s) = tmp_storage();
        // No symbols anywhere named `does_not_exist`.
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::caller", "caller", "function")]);
        b.refs = vec![make_ref("does_not_exist", 1)];
        b.calls = vec![make_call("crate::caller", "also_does_not_exist", 2)];
        b.type_relations = vec![make_type("crate::caller", "still_does_not_exist", 3)];
        insert_extracted_files(&mut s, &[b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 0);
        assert_eq!(stats.resolved_calls, 0);
        assert_eq!(stats.resolved_types, 0);

        let ref_sym: Option<i64> = s
            .connection()
            .query_row("SELECT symbol_id FROM refs", [], |r| r.get(0))
            .unwrap();
        let call_sym: Option<i64> = s
            .connection()
            .query_row("SELECT callee_symbol_id FROM calls", [], |r| r.get(0))
            .unwrap();
        let type_sym: Option<i64> = s
            .connection()
            .query_row("SELECT target_symbol_id FROM types", [], |r| r.get(0))
            .unwrap();
        assert!(ref_sym.is_none(), "unresolved ref must stay NULL");
        assert!(call_sym.is_none(), "unresolved call must stay NULL");
        assert!(type_sym.is_none(), "unresolved type relation must stay NULL");
    }

    #[test]
    fn test_resolve_idempotent_second_run_finds_zero() {
        let (_g, mut s) = tmp_storage();
        let a = rust_file("src/a.rs", vec![make_symbol("crate::x", "x", "function")]);
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::y", "y", "function")]);
        b.refs = vec![make_ref("crate::x", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let first = resolve_cross_file_references(&mut s).expect("first");
        assert_eq!(first.resolved_refs, 1);

        let second = resolve_cross_file_references(&mut s).expect("second");
        assert_eq!(second.resolved_refs, 0, "already-resolved rows must not re-resolve");
    }

    #[test]
    fn test_resolve_bare_name_falls_back_to_same_file() {
        let (_g, mut s) = tmp_storage();
        // File A has a symbol qname `crate::a::helper`. File B references
        // `helper` as a bare name. They are in different files, so the bare
        // name does NOT match in B (verified by the next test).
        // This test: file B has its own `helper` symbol AND a ref to bare `helper`.
        // The ref should resolve to B's helper, not A's.
        let a = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::a::helper", "helper", "function")],
        );
        let mut b = rust_file(
            "src/b.rs",
            vec![make_symbol("crate::b::helper", "helper", "function")],
        );
        b.refs = vec![make_ref("helper", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 1);

        let (sym_id, ref_file_id): (Option<i64>, i64) = s
            .connection()
            .query_row("SELECT symbol_id, file_id FROM refs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let (b_helper_id, b_file_id): (i64, i64) = s
            .connection()
            .query_row(
                "SELECT id, file_id FROM symbols WHERE qualified_name = 'crate::b::helper'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(ref_file_id, b_file_id, "ref should belong to file B");
        assert_eq!(
            sym_id,
            Some(b_helper_id),
            "bare-name ref must resolve to B's helper, not A's"
        );
    }

    #[test]
    fn test_resolve_bare_name_no_match_leaves_null() {
        let (_g, mut s) = tmp_storage();
        // File A has `helper`. File B has a ref to bare `helper` but B has
        // no symbol named `helper`. Bare-name fallback is same-file only,
        // so the ref must stay NULL (cross-file bare names need imports — T3).
        let a = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::a::helper", "helper", "function")],
        );
        let mut b = rust_file("src/b.rs", vec![make_symbol("crate::b::caller", "caller", "function")]);
        b.refs = vec![make_ref("helper", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 0, "bare name in different file must not resolve");

        let sym_id: Option<i64> = s
            .connection()
            .query_row("SELECT symbol_id FROM refs", [], |r| r.get(0))
            .unwrap();
        assert!(sym_id.is_none());
    }

    #[test]
    fn test_resolve_qname_takes_precedence_over_bare_name() {
        let (_g, mut s) = tmp_storage();
        // File B references the qname `crate::a::shadow`. File B ALSO has
        // a same-file symbol named `shadow` (different qname). The ref's
        // raw_name is the FULL qname — phase 1 must match it; phase 2 must
        // not touch it.
        let a = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::a::shadow", "shadow", "function")],
        );
        let mut b = rust_file(
            "src/b.rs",
            vec![make_symbol("crate::b::shadow", "shadow", "function")],
        );
        b.refs = vec![make_ref("crate::a::shadow", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 1);

        let resolved_id: Option<i64> = s
            .connection()
            .query_row("SELECT symbol_id FROM refs", [], |r| r.get(0))
            .unwrap();
        let a_shadow_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::a::shadow'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            resolved_id,
            Some(a_shadow_id),
            "qname match must win over bare-name same-file fallback"
        );
    }

    #[test]
    fn test_resolve_bare_name_calls_same_file() {
        let (_g, mut s) = tmp_storage();
        // Caller and callee are both in the same file; callee_raw_name is bare.
        let mut a = rust_file(
            "src/a.rs",
            vec![
                make_symbol("crate::a::caller", "caller", "function"),
                make_symbol("crate::a::doit", "doit", "function"),
            ],
        );
        a.calls = vec![make_call("crate::a::caller", "doit", 5)];
        insert_extracted_files(&mut s, &[a]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_calls, 1);

        let callee: Option<i64> = s
            .connection()
            .query_row("SELECT callee_symbol_id FROM calls", [], |r| r.get(0))
            .unwrap();
        let doit_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::a::doit'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(callee, Some(doit_id));
    }

    #[test]
    fn test_resolve_bare_name_types_same_file() {
        let (_g, mut s) = tmp_storage();
        // Struct S in file A implements trait T also in file A; target is bare.
        let mut a = rust_file(
            "src/a.rs",
            vec![
                make_symbol("crate::a::S", "S", "struct"),
                make_symbol("crate::a::T", "T", "trait"),
            ],
        );
        a.type_relations = vec![make_type("crate::a::S", "T", 1)];
        insert_extracted_files(&mut s, &[a]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_types, 1);

        let target: Option<i64> = s
            .connection()
            .query_row("SELECT target_symbol_id FROM types", [], |r| r.get(0))
            .unwrap();
        let t_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'crate::a::T'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, Some(t_id));
    }

    #[test]
    fn test_resolve_import_to_top_level_symbol() {
        let (_g, mut s) = tmp_storage();
        // File a.py defines top-level `helper`. File b.py does
        // `from a import helper`, which the extractor emits as
        // raw_path = "a.helper". Resolution must set both
        // resolved_symbol_id (the helper symbol's id) and resolved_file_id
        // (the file a.py's id) on the import row.
        let mut a = ExtractedFile::empty("a.py", LanguageId::Python);
        a.content_hash = [2u8; 32];
        a.size_bytes = 50;
        a.modified_at = 1700000000;
        a.symbols = vec![make_symbol("a.helper", "helper", "function")];

        let mut b = ExtractedFile::empty("b.py", LanguageId::Python);
        b.content_hash = [3u8; 32];
        b.size_bytes = 80;
        b.modified_at = 1700000001;
        b.imports = vec![make_import("a.helper", 1)];

        insert_extracted_files(&mut s, &[a, b]).expect("insert");
        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_imports, 1);

        let (sym, fid): (Option<i64>, Option<i64>) = s
            .connection()
            .query_row(
                "SELECT resolved_symbol_id, resolved_file_id FROM imports",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let (helper_id, a_file_id): (i64, i64) = s
            .connection()
            .query_row(
                "SELECT s.id, s.file_id FROM symbols s WHERE s.qualified_name = 'a.helper'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sym, Some(helper_id), "import must resolve to helper symbol");
        assert_eq!(fid, Some(a_file_id), "import file id must be a.py's id");
    }

    #[test]
    fn test_resolve_import_to_file_id_via_symbol() {
        let (_g, mut s) = tmp_storage();
        // Variant: confirm that even when the imported name is the only
        // symbol in its file, resolved_file_id traces back correctly.
        let a = rust_file(
            "src/util.rs",
            vec![make_symbol("crate::util::compute", "compute", "function")],
        );
        let mut b = rust_file("src/main.rs", vec![]);
        b.imports = vec![make_import("crate::util::compute", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_imports, 1);

        let fid: Option<i64> = s
            .connection()
            .query_row("SELECT resolved_file_id FROM imports", [], |r| r.get(0))
            .unwrap();
        let util_fid: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM files WHERE path = 'src/util.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fid, Some(util_fid));
    }

    #[test]
    fn test_resolve_import_unknown_path_leaves_null() {
        let (_g, mut s) = tmp_storage();
        // No project symbol matches `os.path.join` — typical stdlib import.
        // Must stay NULL on both resolved columns.
        let mut a = ExtractedFile::empty("main.py", LanguageId::Python);
        a.content_hash = [1u8; 32];
        a.size_bytes = 10;
        a.modified_at = 1700000000;
        a.imports = vec![make_import("os.path.join", 1)];
        insert_extracted_files(&mut s, &[a]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_imports, 0);

        let (sym, fid): (Option<i64>, Option<i64>) = s
            .connection()
            .query_row(
                "SELECT resolved_symbol_id, resolved_file_id FROM imports",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(sym.is_none(), "stdlib import must stay NULL");
        assert!(fid.is_none());
    }

    #[test]
    fn test_resolve_import_idempotent() {
        let (_g, mut s) = tmp_storage();
        let a = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::a::foo", "foo", "function")],
        );
        let mut b = rust_file("src/b.rs", vec![]);
        b.imports = vec![make_import("crate::a::foo", 1)];
        insert_extracted_files(&mut s, &[a, b]).expect("insert");

        let first = resolve_cross_file_references(&mut s).expect("first");
        assert_eq!(first.resolved_imports, 1);
        let second = resolve_cross_file_references(&mut s).expect("second");
        assert_eq!(second.resolved_imports, 0, "already-resolved imports must not re-resolve");
    }

    #[test]
    fn test_resolve_duplicate_qname_picks_lowest_id() {
        let (_g, mut s) = tmp_storage();
        // Two files both define top-level `dup`. Resolve should pick the one
        // inserted first (lowest id), per the documented contract.
        let a = rust_file("src/a.rs", vec![make_symbol("dup", "dup", "function")]);
        let b_dupe = rust_file("src/b.rs", vec![make_symbol("dup", "dup", "function")]);
        let mut c = rust_file("src/c.rs", vec![make_symbol("crate::caller", "caller", "function")]);
        c.refs = vec![make_ref("dup", 1)];
        insert_extracted_files(&mut s, &[a, b_dupe, c]).expect("insert");

        let stats = resolve_cross_file_references(&mut s).expect("resolve");
        assert_eq!(stats.resolved_refs, 1);

        let resolved_id: Option<i64> = s
            .connection()
            .query_row("SELECT symbol_id FROM refs", [], |r| r.get(0))
            .unwrap();
        let lowest_id: i64 = s
            .connection()
            .query_row(
                "SELECT MIN(id) FROM symbols WHERE qualified_name = 'dup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved_id, Some(lowest_id), "duplicate qname must resolve to the lowest id");
    }
}
