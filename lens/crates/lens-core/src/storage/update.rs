//! Incremental update — replace records for a subset of files in one
//! transaction so the index never observes a partial state.
//!
//! The complement to [`crate::storage::insert::insert_extracted_files`]:
//! that function assumes a fresh DB (per-path UNIQUE constraint forbids
//! re-insert), this one CASCADE-deletes any pre-existing rows for matching
//! paths and re-inserts. Foreign-key resolution is the caller's job —
//! [`crate::storage::resolve_cross_file_references`] should be invoked
//! after `update_files` to refresh cross-file links touched by the change.
//!
//! ## Why not just DELETE+insert in two transactions?
//!
//! A second reader between the two transactions would see the index with
//! the deleted rows gone and the new rows not yet present. One transaction
//! makes the swap atomic to readers (SQLite serialisable isolation).
//!
//! ## Why CASCADE rather than UPDATE?
//!
//! The schema has FK CASCADE on `files → symbols → refs/calls/imports/types`.
//! Deleting the parent row removes every dependent row. Re-inserting then
//! gets fresh rowids — old rowids are not preserved across an update.
//! Callers who pinned a rowid before update will see it disappear; lens
//! does not currently expose stable cross-update rowids and treats this
//! as expected.

use rusqlite::params;

use crate::error::{LensError, Result};
use crate::extract::ExtractedFile;
use crate::storage::insert::{unix_seconds_now, write_files_into_tx, InsertStats};
use crate::storage::Storage;

/// Per-call counts emitted by [`update_files`]. `files_replaced` counts paths
/// that matched a pre-existing row (CASCADE-deleted then re-inserted);
/// `files_added` counts paths that were not previously present.
/// `files_deleted` counts paths supplied via the `delete_paths` argument that
/// matched a pre-existing row.
///
/// `symbols`, `refs`, `calls`, `imports`, `type_relations` are the raw counts
/// emitted by the inner insert pass for files in the `files` argument — they
/// reflect the NEW records, not the difference vs the previous state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UpdateStats {
    pub files_replaced: u64,
    pub files_added: u64,
    pub files_deleted: u64,
    pub symbols: u64,
    pub refs: u64,
    pub calls: u64,
    pub imports: u64,
    pub type_relations: u64,
}

impl UpdateStats {
    fn from_insert(ins: InsertStats, files_replaced: u64, files_added: u64, files_deleted: u64) -> Self {
        Self {
            files_replaced,
            files_added,
            files_deleted,
            symbols: ins.symbols,
            refs: ins.refs,
            calls: ins.calls,
            imports: ins.imports,
            type_relations: ins.type_relations,
        }
    }
}

/// Replace records for `files` and remove records for `delete_paths`, in one
/// transaction.
///
/// For each path in `files`: if a row with that path exists, it is
/// CASCADE-deleted (which removes its symbols, refs, calls, imports, types via
/// the schema's FK CASCADE rules). The record is then re-inserted from the
/// `ExtractedFile` payload. If no row exists, the record is inserted fresh.
///
/// For each path in `delete_paths`: if a row exists it is CASCADE-deleted; if
/// not, the path is silently skipped (idempotent).
///
/// Cross-file foreign keys (`refs.symbol_id`, `calls.callee_symbol_id`, etc.)
/// are left NULL by this function — call
/// [`crate::storage::resolve_cross_file_references`] afterwards to refresh
/// them. Existing pre-resolved FKs that point at a deleted symbol get NULL'd
/// by `ON DELETE SET NULL` (imports) or CASCADE-removed (refs/calls/types).
///
/// Errors abort the whole transaction; partial updates never persist.
pub fn update_files(
    storage: &mut Storage,
    files: &[ExtractedFile],
    delete_paths: &[&str],
) -> Result<UpdateStats> {
    let now = unix_seconds_now();
    let tx = storage.transaction()?;

    let (replaced, deleted) = {
        let mut del_by_path = tx
            .prepare("DELETE FROM files WHERE path = ?1")
            .map_err(|e| LensError::other(format!("prepare delete files: {e}")))?;

        let mut replaced = 0u64;
        for ef in files {
            let n = del_by_path
                .execute(params![&ef.relative_path])
                .map_err(|e| {
                    LensError::other(format!("delete file {}: {e}", ef.relative_path))
                })?;
            replaced += n as u64;
        }

        let mut deleted = 0u64;
        for p in delete_paths {
            let n = del_by_path
                .execute(params![p])
                .map_err(|e| LensError::other(format!("delete file {p}: {e}")))?;
            deleted += n as u64;
        }

        (replaced, deleted)
    };

    // Re-insert all files in the same transaction. write_files_into_tx never
    // commits — that is the caller's job below.
    let ins = write_files_into_tx(&tx, files, now)?;

    tx.commit()
        .map_err(|e| LensError::other(format!("commit update transaction: {e}")))?;

    let added = ins.files.saturating_sub(replaced);
    Ok(UpdateStats::from_insert(ins, replaced, added, deleted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedFile, ExtractedSymbol};
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

    fn make_symbol(qname: &str, name: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: "function".into(),
            start_line: 1,
            start_col: 0,
            end_line: 2,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 10,
            signature: None,
            visibility: None,
            parent_qualified_name: None,
            doc_comment: None,
        }
    }

    fn rust_file(path: &str, syms: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Rust);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 100;
        ef.modified_at = 1700000000;
        ef.symbols = syms;
        ef
    }

    #[test]
    fn test_update_files_inserts_when_path_is_new() {
        let (_g, mut s) = tmp_storage();
        let ef = rust_file("src/a.rs", vec![make_symbol("a::foo", "foo")]);
        let stats = update_files(&mut s, &[ef], &[]).unwrap();
        assert_eq!(stats.files_added, 1);
        assert_eq!(stats.files_replaced, 0);
        assert_eq!(stats.files_deleted, 0);
        assert_eq!(stats.symbols, 1);

        let n: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_update_files_replaces_existing_path_via_cascade() {
        let (_g, mut s) = tmp_storage();
        // First version of src/a.rs has TWO symbols.
        let v1 = rust_file(
            "src/a.rs",
            vec![make_symbol("a::foo", "foo"), make_symbol("a::bar", "bar")],
        );
        insert_extracted_files(&mut s, &[v1]).unwrap();
        let pre_syms: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pre_syms, 2);

        // Second version has ONE symbol (the rename of foo).
        let v2 = rust_file("src/a.rs", vec![make_symbol("a::baz", "baz")]);
        let stats = update_files(&mut s, &[v2], &[]).unwrap();
        assert_eq!(stats.files_replaced, 1);
        assert_eq!(stats.files_added, 0);
        assert_eq!(stats.symbols, 1);

        // CASCADE must have wiped the old symbols. Only the new one remains.
        let post_syms: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(post_syms, 1);
        let qname: String = s
            .connection()
            .query_row("SELECT qualified_name FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(qname, "a::baz");

        // The files row must persist with a SINGLE entry for src/a.rs.
        let files: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1);
    }

    #[test]
    fn test_update_files_deletes_paths_supplied_in_delete_paths() {
        let (_g, mut s) = tmp_storage();
        let v1 = rust_file("src/keep.rs", vec![make_symbol("keep::k", "k")]);
        let v2 = rust_file("src/gone.rs", vec![make_symbol("gone::g", "g")]);
        insert_extracted_files(&mut s, &[v1, v2]).unwrap();

        let stats = update_files(&mut s, &[], &["src/gone.rs"]).unwrap();
        assert_eq!(stats.files_deleted, 1);
        assert_eq!(stats.files_replaced, 0);
        assert_eq!(stats.files_added, 0);

        let remaining_paths: Vec<String> = s
            .connection()
            .prepare("SELECT path FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(remaining_paths, vec!["src/keep.rs"]);
    }

    #[test]
    fn test_update_files_delete_path_absent_is_idempotent_noop() {
        let (_g, mut s) = tmp_storage();
        let stats = update_files(&mut s, &[], &["does/not/exist.rs"]).unwrap();
        assert_eq!(stats.files_deleted, 0);
        assert_eq!(stats.files_added, 0);
        assert_eq!(stats.files_replaced, 0);
    }

    #[test]
    fn test_update_files_atomic_rollback_on_failure() {
        // Force a failure mid-update: delete a file (SET NULL on imports
        // only — refs/calls cascade), then submit a NEW file whose insert
        // would succeed. We then provoke an error by submitting two
        // ExtractedFiles with the SAME relative_path — the first DELETE
        // reaps any prior row (none), then DELETE for the second iteration
        // reaps the row inserted at step 1 (counted as another replace),
        // then both get inserted. Wait — that's actually fine, both
        // deletes succeed before either insert.
        //
        // Simpler approach: produce a payload that will fail at insert
        // time. The insert path errors when a `call` references a
        // caller_qualified_name not in the symbol table. We exploit that
        // to cause a transaction abort, then verify the prior state is
        // still intact.
        let (_g, mut s) = tmp_storage();
        let intact = rust_file("src/intact.rs", vec![make_symbol("intact::ok", "ok")]);
        insert_extracted_files(&mut s, &[intact]).unwrap();

        let mut bad = rust_file("src/bad.rs", vec![]);
        bad.calls.push(crate::extract::ExtractedCall {
            caller_qualified_name: "no::such::caller".into(),
            callee_raw_name: "x".into(),
            line: 1,
            col: 0,
        });
        let result = update_files(&mut s, &[bad], &["src/intact.rs"]);
        assert!(result.is_err(), "bad call payload must abort the transaction");

        // The intact file must still be present — the DELETE was inside the
        // aborted transaction and rolled back.
        let intact_count: i64 = s
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = 'src/intact.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(intact_count, 1, "rollback failed — intact file was deleted");
        let intact_syms: i64 = s
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'intact::ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(intact_syms, 1);
    }

    #[test]
    fn test_update_files_replace_resets_indexed_at() {
        // The indexed_at timestamp must be refreshed on replace — useful for
        // diagnostic queries asking "when was this file last indexed?".
        let (_g, mut s) = tmp_storage();
        let v1 = rust_file("src/a.rs", vec![]);
        insert_extracted_files(&mut s, &[v1]).unwrap();
        let t1: i64 = s
            .connection()
            .query_row("SELECT indexed_at FROM files WHERE path = 'src/a.rs'", [], |r| r.get(0))
            .unwrap();

        // Sleep 1 second to ensure unix_seconds_now() advances.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let v2 = rust_file("src/a.rs", vec![]);
        update_files(&mut s, &[v2], &[]).unwrap();
        let t2: i64 = s
            .connection()
            .query_row("SELECT indexed_at FROM files WHERE path = 'src/a.rs'", [], |r| r.get(0))
            .unwrap();
        assert!(t2 > t1, "indexed_at must advance on replace: t1={t1} t2={t2}");
    }

    #[test]
    fn test_update_files_mixed_replace_and_add_in_one_call() {
        let (_g, mut s) = tmp_storage();
        let pre = rust_file("src/old.rs", vec![make_symbol("old::x", "x")]);
        insert_extracted_files(&mut s, &[pre]).unwrap();

        let replace = rust_file("src/old.rs", vec![make_symbol("old::y", "y")]);
        let new = rust_file("src/new.rs", vec![make_symbol("new::z", "z")]);
        let stats = update_files(&mut s, &[replace, new], &[]).unwrap();
        assert_eq!(stats.files_replaced, 1);
        assert_eq!(stats.files_added, 1);
        assert_eq!(stats.symbols, 2);
    }

    #[test]
    fn test_update_files_empty_inputs_is_noop() {
        let (_g, mut s) = tmp_storage();
        let stats = update_files(&mut s, &[], &[]).unwrap();
        assert_eq!(stats, UpdateStats::default());
    }

    #[test]
    fn test_update_stats_default_is_all_zero() {
        let s = UpdateStats::default();
        assert_eq!(s.files_replaced, 0);
        assert_eq!(s.files_added, 0);
        assert_eq!(s.files_deleted, 0);
        assert_eq!(s.symbols, 0);
        assert_eq!(s.refs, 0);
        assert_eq!(s.calls, 0);
        assert_eq!(s.imports, 0);
        assert_eq!(s.type_relations, 0);
    }
}
