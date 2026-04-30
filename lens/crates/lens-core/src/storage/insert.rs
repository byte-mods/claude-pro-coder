//! Bulk-insert pass — write [`ExtractedFile`] records into the SQLite tables
//! defined by [`crate::storage::schema`].
//!
//! Phase 1 (this module): one transaction, prepared statements, deterministic
//! order. FK columns whose target depends on cross-file lookups are left NULL
//! and resolved by the second pass in [`crate::storage::resolve`]. Three
//! columns whose target is always *same-file* are resolved here at insert time
//! using the per-file `qualified_name → rowid` map:
//!
//! - `calls.caller_symbol_id` (NOT NULL in schema)
//! - `types.symbol_id` (NOT NULL in schema)
//! - `symbols.parent_symbol_id` (nullable, but the parent is always in the
//!   same file by Rust/Python nesting semantics — resolving here keeps the
//!   resolve pass focused on cross-file lookups)
//!
//! ## Concurrency
//!
//! Insert is sequential. SQLite + WAL handles concurrent readers; lens does not
//! need concurrent writers in v1. The pipeline that produces [`ExtractedFile`]
//! values runs in parallel ([`crate::extract::pipeline::run`]); the bulk-insert
//! consumes that output serially.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::error::{LensError, Result};
use crate::extract::ExtractedFile;
use crate::storage::Storage;

/// Per-table row counts written by [`insert_extracted_files`]. Useful for
/// tests and CLI summary output.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InsertStats {
    pub files: u64,
    pub symbols: u64,
    pub refs: u64,
    pub calls: u64,
    pub imports: u64,
    pub type_relations: u64,
}

/// Bulk-insert `files` into the storage's SQLite database inside a single
/// transaction. The `indexed_at` timestamp is set to `now` for every file in
/// the batch.
///
/// Same-file foreign keys (`calls.caller_symbol_id`, `types.symbol_id`,
/// `symbols.parent_symbol_id`) are resolved at insert time. Cross-file foreign
/// keys (`refs.symbol_id`, `calls.callee_symbol_id`, `types.target_symbol_id`,
/// `imports.resolved_*`) are left NULL and filled in by
/// [`crate::storage::resolve`].
///
/// Errors abort the transaction and propagate; partial inserts never persist.
pub fn insert_extracted_files(
    storage: &mut Storage,
    files: &[ExtractedFile],
) -> Result<InsertStats> {
    let now = unix_seconds_now();
    let tx = storage.transaction()?;
    let stats = write_files_into_tx(&tx, files, now)?;
    tx.commit()
        .map_err(|e| LensError::other(format!("commit insert transaction: {e}")))?;
    Ok(stats)
}

/// Bulk-insert `files` inside an externally-supplied transaction. Used by both
/// [`insert_extracted_files`] (which opens its own transaction) and
/// [`crate::storage::update::update_files`] (which combines DELETE+insert in
/// one transaction so the index is never observed in a partial state).
///
/// The caller owns transaction lifecycle (begin / commit / rollback).
pub(crate) fn write_files_into_tx(
    tx: &rusqlite::Transaction<'_>,
    files: &[ExtractedFile],
    now: i64,
) -> Result<InsertStats> {
    let mut stats = InsertStats::default();

    {
        let mut ins_file = tx
            .prepare(
                "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(|e| LensError::other(format!("prepare insert files: {e}")))?;
        let mut ins_symbol = tx
            .prepare(
                "INSERT INTO symbols (file_id, qualified_name, name, kind,
                                       start_line, start_col, end_line, end_col,
                                       body_start_byte, body_end_byte,
                                       signature, visibility, parent_symbol_id,
                                       doc_comment)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            )
            .map_err(|e| LensError::other(format!("prepare insert symbols: {e}")))?;
        let mut ins_ref = tx
            .prepare(
                "INSERT INTO refs (symbol_id, file_id, line, col, end_line, end_col, kind, raw_name)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )
            .map_err(|e| LensError::other(format!("prepare insert refs: {e}")))?;
        let mut ins_call = tx
            .prepare(
                "INSERT INTO calls (caller_symbol_id, callee_symbol_id, callee_raw_name, file_id, line, col)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )
            .map_err(|e| LensError::other(format!("prepare insert calls: {e}")))?;
        let mut ins_import = tx
            .prepare(
                "INSERT INTO imports (file_id, raw_path, resolved_file_id, resolved_symbol_id, alias, line)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )
            .map_err(|e| LensError::other(format!("prepare insert imports: {e}")))?;
        let mut ins_type = tx
            .prepare(
                "INSERT INTO types (symbol_id, relation, target_symbol_id, target_raw_name, file_id, line)
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )
            .map_err(|e| LensError::other(format!("prepare insert types: {e}")))?;
        let mut upd_parent = tx
            .prepare("UPDATE symbols SET parent_symbol_id = ?1 WHERE id = ?2")
            .map_err(|e| LensError::other(format!("prepare update parent: {e}")))?;

        for ef in files {
            ins_file
                .execute(params![
                    &ef.relative_path,
                    ef.language.as_str(),
                    &ef.content_hash[..],
                    ef.size_bytes as i64,
                    ef.modified_at,
                    now,
                ])
                .map_err(|e| {
                    LensError::other(format!("insert file {}: {e}", ef.relative_path))
                })?;
            let file_id = tx.last_insert_rowid();
            stats.files += 1;

            // qualified_name -> rowid for in-file FK resolution at insert time.
            let mut sym_id: HashMap<&str, i64> = HashMap::with_capacity(ef.symbols.len());
            // (child_rowid, parent_qname) — fixed up after the file's symbol
            // map is fully built so order of emission doesn't matter.
            let mut pending_parents: Vec<(i64, &str)> = Vec::new();
            for sym in &ef.symbols {
                ins_symbol
                    .execute(params![
                        file_id,
                        &sym.qualified_name,
                        &sym.name,
                        &sym.kind,
                        sym.start_line,
                        sym.start_col,
                        sym.end_line,
                        sym.end_col,
                        sym.body_start_byte,
                        sym.body_end_byte,
                        sym.signature.as_deref(),
                        sym.visibility.as_deref(),
                        Option::<i64>::None,
                        sym.doc_comment.as_deref(),
                    ])
                    .map_err(|e| {
                        LensError::other(format!(
                            "insert symbol {} in {}: {e}",
                            sym.qualified_name, ef.relative_path
                        ))
                    })?;
                let id = tx.last_insert_rowid();
                sym_id.insert(sym.qualified_name.as_str(), id);
                if let Some(pq) = &sym.parent_qualified_name {
                    pending_parents.push((id, pq.as_str()));
                }
                stats.symbols += 1;
            }

            // Resolve parent_symbol_id for symbols whose parent_qualified_name
            // refers to a same-file symbol. Parents in other files (rare in
            // practice) stay NULL.
            for (child_id, pqname) in &pending_parents {
                if let Some(parent_id) = sym_id.get(pqname).copied() {
                    upd_parent
                        .execute(params![parent_id, child_id])
                        .map_err(|e| {
                            LensError::other(format!(
                                "set parent_symbol_id={parent_id} on row {child_id}: {e}"
                            ))
                        })?;
                }
            }

            for r in &ef.refs {
                ins_ref
                    .execute(params![
                        Option::<i64>::None,
                        file_id,
                        r.line,
                        r.col,
                        r.end_line,
                        r.end_col,
                        &r.kind,
                        &r.raw_name,
                    ])
                    .map_err(|e| {
                        LensError::other(format!("insert ref in {}: {e}", ef.relative_path))
                    })?;
                stats.refs += 1;
            }

            for c in &ef.calls {
                let caller_id = sym_id
                    .get(c.caller_qualified_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        LensError::other(format!(
                            "call in {} has unknown caller {} — extractor invariant violated",
                            ef.relative_path, c.caller_qualified_name
                        ))
                    })?;
                ins_call
                    .execute(params![
                        caller_id,
                        Option::<i64>::None,
                        &c.callee_raw_name,
                        file_id,
                        c.line,
                        c.col,
                    ])
                    .map_err(|e| {
                        LensError::other(format!("insert call in {}: {e}", ef.relative_path))
                    })?;
                stats.calls += 1;
            }

            for imp in &ef.imports {
                ins_import
                    .execute(params![
                        file_id,
                        &imp.raw_path,
                        Option::<i64>::None,
                        Option::<i64>::None,
                        imp.alias.as_deref(),
                        imp.line,
                    ])
                    .map_err(|e| {
                        LensError::other(format!(
                            "insert import in {}: {e}",
                            ef.relative_path
                        ))
                    })?;
                stats.imports += 1;
            }

            for t in &ef.type_relations {
                let symbol_id = sym_id
                    .get(t.symbol_qualified_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        LensError::other(format!(
                            "type relation in {} has unknown owner {} — extractor invariant violated",
                            ef.relative_path, t.symbol_qualified_name
                        ))
                    })?;
                ins_type
                    .execute(params![
                        symbol_id,
                        &t.relation,
                        Option::<i64>::None,
                        &t.target_raw_name,
                        file_id,
                        t.line,
                    ])
                    .map_err(|e| {
                        LensError::other(format!(
                            "insert type relation in {}: {e}",
                            ef.relative_path
                        ))
                    })?;
                stats.type_relations += 1;
            }
        }
    } // statements drop here, freeing the caller's tx for commit/rollback.

    Ok(stats)
}

pub fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{
        ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
        ExtractedTypeRel,
    };
    use crate::lang::LanguageId;
    use rusqlite::params;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn make_symbol(qname: &str, name: &str, kind: &str, parent: Option<&str>) -> ExtractedSymbol {
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
            parent_qualified_name: parent.map(str::to_string),
            doc_comment: None,
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
    fn test_insert_one_file_writes_files_row() {
        let (_g, mut s) = tmp_storage();
        let ef = rust_file("src/a.rs", vec![]);
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.symbols, 0);

        let count: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let path: String = s
            .connection()
            .query_row("SELECT path FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "src/a.rs");
    }

    #[test]
    fn test_insert_one_file_writes_symbol_row_with_file_fk() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![make_symbol("foo", "foo", "function", None)]);
        ef.symbols[0].signature = Some("fn foo()".into());
        ef.symbols[0].visibility = Some("pub".into());
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.symbols, 1);

        let (qname, name, kind, sig, vis, parent_id, file_id): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            i64,
        ) = s
            .connection()
            .query_row(
                "SELECT qualified_name, name, kind, signature, visibility, parent_symbol_id, file_id FROM symbols",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            )
            .unwrap();
        assert_eq!(qname, "foo");
        assert_eq!(name, "foo");
        assert_eq!(kind, "function");
        assert_eq!(sig.as_deref(), Some("fn foo()"));
        assert_eq!(vis.as_deref(), Some("pub"));
        assert!(parent_id.is_none(), "top-level symbol has no parent");
        assert!(file_id > 0);
    }

    #[test]
    fn test_insert_resolves_parent_symbol_id_for_same_file_parent() {
        let (_g, mut s) = tmp_storage();
        let parent = make_symbol("crate::Foo", "Foo", "struct", None);
        let child = make_symbol("crate::Foo::bar", "bar", "method", Some("crate::Foo"));
        let ef = rust_file("src/a.rs", vec![parent, child]);
        insert_extracted_files(&mut s, &[ef]).unwrap();

        let (parent_id, child_parent_id): (i64, i64) = s
            .connection()
            .query_row(
                "SELECT
                    (SELECT id FROM symbols WHERE qualified_name = 'crate::Foo'),
                    (SELECT parent_symbol_id FROM symbols WHERE qualified_name = 'crate::Foo::bar')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(child_parent_id, parent_id, "child must point at parent rowid");
    }

    #[test]
    fn test_insert_parent_resolution_works_when_child_emitted_before_parent() {
        // Order-independence check: emit child first, parent second.
        let (_g, mut s) = tmp_storage();
        let child = make_symbol("crate::Foo::bar", "bar", "method", Some("crate::Foo"));
        let parent = make_symbol("crate::Foo", "Foo", "struct", None);
        let ef = rust_file("src/a.rs", vec![child, parent]);
        insert_extracted_files(&mut s, &[ef]).unwrap();

        let (parent_id, child_parent_id): (i64, i64) = s
            .connection()
            .query_row(
                "SELECT
                    (SELECT id FROM symbols WHERE qualified_name = 'crate::Foo'),
                    (SELECT parent_symbol_id FROM symbols WHERE qualified_name = 'crate::Foo::bar')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(child_parent_id, parent_id);
    }

    #[test]
    fn test_insert_leaves_parent_null_when_parent_qname_not_in_same_file() {
        // Parent is in a different file in this synthetic — the resolver
        // (T11) may pick it up, but at insert time we leave the FK NULL.
        let (_g, mut s) = tmp_storage();
        let other_file = rust_file(
            "src/other.rs",
            vec![make_symbol("crate::other::Foo", "Foo", "struct", None)],
        );
        let main = rust_file(
            "src/a.rs",
            vec![make_symbol(
                "crate::a::bar",
                "bar",
                "function",
                Some("crate::other::Foo"),
            )],
        );
        insert_extracted_files(&mut s, &[other_file, main]).unwrap();

        let parent_id: Option<i64> = s
            .connection()
            .query_row(
                "SELECT parent_symbol_id FROM symbols WHERE qualified_name = 'crate::a::bar'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(parent_id.is_none(), "cross-file parent left NULL by insert");
    }

    #[test]
    fn test_insert_resolves_caller_symbol_id_in_file() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file(
            "src/a.rs",
            vec![
                make_symbol("crate::caller", "caller", "function", None),
                make_symbol("crate::callee", "callee", "function", None),
            ],
        );
        ef.calls.push(ExtractedCall {
            caller_qualified_name: "crate::caller".into(),
            callee_raw_name: "callee".into(),
            line: 5,
            col: 10,
        });
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.calls, 1);

        let (caller_id, callee_id): (i64, Option<i64>) = s
            .connection()
            .query_row(
                "SELECT caller_symbol_id, callee_symbol_id FROM calls",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let caller_qname: String = s
            .connection()
            .query_row(
                "SELECT qualified_name FROM symbols WHERE id = ?1",
                params![caller_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(caller_qname, "crate::caller");
        assert!(callee_id.is_none(), "callee_symbol_id deferred to resolve pass");
    }

    #[test]
    fn test_insert_resolves_type_relation_owner_in_file() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::Foo", "Foo", "struct", None)],
        );
        ef.type_relations.push(ExtractedTypeRel {
            symbol_qualified_name: "crate::Foo".into(),
            relation: "field_type".into(),
            target_raw_name: "Bar".into(),
            line: 3,
        });
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.type_relations, 1);

        let (owner_id, target_id, target_raw): (i64, Option<i64>, String) = s
            .connection()
            .query_row(
                "SELECT symbol_id, target_symbol_id, target_raw_name FROM types",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        let owner_qname: String = s
            .connection()
            .query_row(
                "SELECT qualified_name FROM symbols WHERE id = ?1",
                params![owner_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner_qname, "crate::Foo");
        assert!(target_id.is_none(), "target_symbol_id deferred to resolve pass");
        assert_eq!(target_raw, "Bar");
    }

    #[test]
    fn test_insert_writes_refs_with_null_symbol_id() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![]);
        ef.refs.push(ExtractedRef {
            raw_name: "Bar".into(),
            kind: "type".into(),
            line: 1,
            col: 5,
            end_line: 1,
            end_col: 8,
        });
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.refs, 1);

        let (sym_id, raw): (Option<i64>, String) = s
            .connection()
            .query_row("SELECT symbol_id, raw_name FROM refs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert!(sym_id.is_none(), "ref symbol_id deferred to resolve pass");
        assert_eq!(raw, "Bar");
    }

    #[test]
    fn test_insert_writes_imports_with_null_resolved_columns() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![]);
        ef.imports.push(ExtractedImport {
            raw_path: "std::collections::HashMap".into(),
            alias: Some("HM".into()),
            line: 1,
        });
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.imports, 1);

        let (rf, rs, alias): (Option<i64>, Option<i64>, Option<String>) = s
            .connection()
            .query_row(
                "SELECT resolved_file_id, resolved_symbol_id, alias FROM imports",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(rf.is_none() && rs.is_none(), "import resolution deferred");
        assert_eq!(alias.as_deref(), Some("HM"));
    }

    #[test]
    fn test_insert_handles_multiple_files_writing_independently() {
        let (_g, mut s) = tmp_storage();
        let ef1 = rust_file("src/a.rs", vec![make_symbol("a::foo", "foo", "function", None)]);
        let mut ef2 = ExtractedFile::empty("src/b.py", LanguageId::Python);
        ef2.content_hash = [2u8; 32];
        ef2.size_bytes = 50;
        ef2.modified_at = 1700000001;
        ef2.symbols.push(make_symbol("b.bar", "bar", "function", None));
        let stats = insert_extracted_files(&mut s, &[ef1, ef2]).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.symbols, 2);

        let langs: Vec<String> = s
            .connection()
            .prepare("SELECT language FROM files ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(langs, vec!["rust", "python"]);
    }

    #[test]
    fn test_insert_returns_per_table_counts() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file(
            "src/a.rs",
            vec![make_symbol("crate::caller", "caller", "function", None)],
        );
        ef.refs.push(ExtractedRef {
            raw_name: "X".into(),
            kind: "type".into(),
            line: 1,
            col: 0,
            end_line: 1,
            end_col: 1,
        });
        ef.refs.push(ExtractedRef {
            raw_name: "Y".into(),
            kind: "type".into(),
            line: 2,
            col: 0,
            end_line: 2,
            end_col: 1,
        });
        ef.calls.push(ExtractedCall {
            caller_qualified_name: "crate::caller".into(),
            callee_raw_name: "z".into(),
            line: 3,
            col: 0,
        });
        ef.imports.push(ExtractedImport {
            raw_path: "x::y".into(),
            alias: None,
            line: 1,
        });
        let stats = insert_extracted_files(&mut s, &[ef]).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.symbols, 1);
        assert_eq!(stats.refs, 2);
        assert_eq!(stats.calls, 1);
        assert_eq!(stats.imports, 1);
        assert_eq!(stats.type_relations, 0);
    }

    #[test]
    fn test_insert_rolls_back_on_unique_path_conflict() {
        let (_g, mut s) = tmp_storage();
        let ef1 = rust_file("src/dup.rs", vec![make_symbol("a::foo", "foo", "function", None)]);
        let ef2 = rust_file("src/dup.rs", vec![make_symbol("a::bar", "bar", "function", None)]);
        let r = insert_extracted_files(&mut s, &[ef1, ef2]);
        assert!(r.is_err(), "expected UNIQUE constraint to fail second insert");

        // No partial state should remain — the whole tx rolls back.
        let count: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "transaction must roll back; no rows persisted");
        let scount: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(scount, 0, "symbols must also roll back");
    }

    #[test]
    fn test_insert_errors_when_call_caller_qname_not_in_file() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![make_symbol("a::other", "other", "function", None)]);
        ef.calls.push(ExtractedCall {
            caller_qualified_name: "a::missing".into(),
            callee_raw_name: "z".into(),
            line: 1,
            col: 0,
        });
        let r = insert_extracted_files(&mut s, &[ef]);
        assert!(r.is_err(), "unknown caller qname must surface as error");
        let msg = format!("{}", r.unwrap_err());
        assert!(msg.contains("a::missing"), "error must name missing qname: {msg}");
    }

    #[test]
    fn test_insert_errors_when_type_relation_owner_qname_not_in_file() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![make_symbol("a::Foo", "Foo", "struct", None)]);
        ef.type_relations.push(ExtractedTypeRel {
            symbol_qualified_name: "a::Missing".into(),
            relation: "field_type".into(),
            target_raw_name: "X".into(),
            line: 1,
        });
        let r = insert_extracted_files(&mut s, &[ef]);
        assert!(r.is_err());
        let msg = format!("{}", r.unwrap_err());
        assert!(msg.contains("a::Missing"));
    }

    #[test]
    fn test_insert_persists_content_hash_and_size_and_mtime() {
        let (_g, mut s) = tmp_storage();
        let mut ef = rust_file("src/a.rs", vec![]);
        ef.content_hash = [0xAB; 32];
        ef.size_bytes = 12345;
        ef.modified_at = 1_700_000_500;
        insert_extracted_files(&mut s, &[ef]).unwrap();

        let (hash, size, mt): (Vec<u8>, i64, i64) = s
            .connection()
            .query_row(
                "SELECT content_hash, size_bytes, modified_at FROM files",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(hash.len(), 32);
        assert!(hash.iter().all(|b| *b == 0xAB), "hash bytes round-trip");
        assert_eq!(size, 12345);
        assert_eq!(mt, 1_700_000_500);
    }

    #[test]
    fn test_insert_writes_indexed_at_close_to_now() {
        let (_g, mut s) = tmp_storage();
        let before = unix_seconds_now();
        let ef = rust_file("src/a.rs", vec![]);
        insert_extracted_files(&mut s, &[ef]).unwrap();
        let after = unix_seconds_now();

        let indexed_at: i64 = s
            .connection()
            .query_row("SELECT indexed_at FROM files", [], |r| r.get(0))
            .unwrap();
        assert!(
            indexed_at >= before && indexed_at <= after,
            "indexed_at {indexed_at} not in [{before}, {after}]"
        );
    }

    #[test]
    fn test_insert_empty_batch_is_noop() {
        let (_g, mut s) = tmp_storage();
        let stats = insert_extracted_files(&mut s, &[]).unwrap();
        assert_eq!(stats, InsertStats::default());

        let count: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
