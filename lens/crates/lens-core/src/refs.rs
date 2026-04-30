//! `lens refs <symbol>` — list callers (and other reference sites) of a symbol.
//!
//! `follow` returns the *definition* with a tiny caller preview. `refs` answers
//! the inverse question: "if I change this, what breaks?" — exhaustive caller
//! enumeration up to a configurable limit, deterministic by smallest
//! `caller_symbol_id` ascending.
//!
//! The data source is `calls.callee_symbol_id = ?` plus `refs.symbol_id = ?`
//! (cross-file resolved at index time). Symbol resolution from a free-form
//! string is the caller's job (use [`crate::resolve_symbol_to_id`]).

use rusqlite::OptionalExtension;

use crate::error::{LensError, Result};
use crate::query::QueryNode;
use crate::storage::Storage;

/// One reference site to a symbol — either a call (caller_symbol → callee) or
/// a non-call reference (e.g. a type mention). Carries the file:line and the
/// owning caller symbol when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    /// The symbol that contains the reference (`None` for module-level refs
    /// not enclosed in any symbol).
    pub caller: Option<QueryNode>,
    /// Reference kind — `"call"` for entries from `calls`, otherwise the
    /// `refs.kind` column value (typically `"identifier"`, `"type"`).
    pub kind: String,
    /// File path of the reference site.
    pub file_path: String,
    /// Line of the reference site (1-indexed).
    pub line: i64,
    /// Column of the reference site (0-indexed).
    pub col: i64,
}

/// What `lens refs <symbol>` returns: the focus symbol metadata + an ordered
/// list of reference sites + a flag indicating whether more sites exist past
/// the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefsResult {
    pub focus: QueryNode,
    pub sites: Vec<RefSite>,
    /// `true` when the underlying query produced more sites than `limit`
    /// returned. Lets the renderer say "showing N of M" without re-running
    /// a count query.
    pub truncated: bool,
}

/// Maximum hard cap on caller enumeration. Even when the user passes a huge
/// `--limit`, we won't load more than this in one query — keeps memory
/// bounded on pathological hot symbols.
pub const HARD_LIMIT: u32 = 10_000;

/// Look up a symbol by id and return its reference sites.
///
/// `limit` is a soft cap (the caller's `--limit` flag); the function silently
/// clamps to [`HARD_LIMIT`]. Sites are ordered by:
///   1. caller `symbol_id` ascending (so the same caller groups together);
///   2. then file path ascending;
///   3. then line ascending.
///
/// Returns `Ok(None)` when `sid` is not present in storage.
///
/// # Errors
/// SQLite read failures only.
pub fn list_refs(storage: &Storage, sid: i64, limit: u32) -> Result<Option<RefsResult>> {
    let conn = storage.connection();

    // 1. Focus row. Same shape as follow's first query so renderers can
    //    print a consistent header.
    let focus_row = conn
        .query_row(
            "SELECT s.qualified_name, s.name, s.kind, s.start_line, f.path
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE s.id = ?1",
            rusqlite::params![sid],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| LensError::other(format!("refs: query focus row: {e}")))?;

    let Some((qname, name, kind, start_line, file_path)) = focus_row else {
        return Ok(None);
    };

    let focus = QueryNode {
        symbol_id: sid,
        qualified_name: qname,
        name,
        kind,
        file_path,
        start_line,
        is_seed: true,
    };

    // 2. Pull sites — UNION of calls + non-call refs. We grab `limit + 1` so
    //    we can detect whether more results exist past the cap without a
    //    separate COUNT query.
    let effective_limit = limit.min(HARD_LIMIT);
    let probe_limit = effective_limit.saturating_add(1);

    // Two-step load: calls first (resolved by callee_symbol_id), then non-call
    // refs (resolved by symbol_id). They share the QueryNode-shape result via
    // `RefSite`; the caller column is `Option<QueryNode>` because module-level
    // refs have no enclosing symbol.
    //
    // We deliberately keep these as two separate queries rather than a SQL
    // UNION because the columns differ (calls have caller_symbol_id; refs
    // have symbol_id of the *target*, not the caller). Joining them in app
    // code keeps the SQL straightforward and the ordering stable.
    let mut sites = load_call_sites(storage, sid, probe_limit)?;
    if sites.len() < probe_limit as usize {
        let remaining = probe_limit - sites.len() as u32;
        sites.extend(load_non_call_ref_sites(storage, sid, remaining)?);
    }

    let truncated = sites.len() as u32 > effective_limit;
    sites.truncate(effective_limit as usize);

    // Final stable ordering: by caller symbol_id (None last), then file, then line.
    sites.sort_by(|a, b| {
        let a_id = a.caller.as_ref().map(|c| c.symbol_id).unwrap_or(i64::MAX);
        let b_id = b.caller.as_ref().map(|c| c.symbol_id).unwrap_or(i64::MAX);
        a_id.cmp(&b_id)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.line.cmp(&b.line))
    });

    Ok(Some(RefsResult { focus, sites, truncated }))
}

fn load_call_sites(storage: &Storage, sid: i64, limit: u32) -> Result<Vec<RefSite>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.qualified_name, s.name, s.kind, sf.path, s.start_line,
                    cf.path, c.line, c.col
             FROM calls c
             JOIN symbols s ON s.id = c.caller_symbol_id
             JOIN files sf ON sf.id = s.file_id
             JOIN files cf ON cf.id = c.file_id
             WHERE c.callee_symbol_id = ?1
             ORDER BY s.id ASC, c.line ASC
             LIMIT ?2",
        )
        .map_err(|e| LensError::other(format!("refs: prepare call sites: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![sid, limit as i64], |row| {
            let caller = QueryNode {
                symbol_id: row.get(0)?,
                qualified_name: row.get(1)?,
                name: row.get(2)?,
                kind: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                is_seed: false,
            };
            Ok(RefSite {
                caller: Some(caller),
                kind: "call".to_string(),
                file_path: row.get(6)?,
                line: row.get(7)?,
                col: row.get(8)?,
            })
        })
        .map_err(|e| LensError::other(format!("refs: query call sites: {e}")))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("refs: collect call sites: {e}")))
}

fn load_non_call_ref_sites(storage: &Storage, sid: i64, limit: u32) -> Result<Vec<RefSite>> {
    let conn = storage.connection();
    // refs.symbol_id points at the *target* symbol, with line/col at the use
    // site. The enclosing caller is best-effort: we look up the symbol whose
    // body byte range covers the ref byte range in the same file. If none
    // matches (module-level reference), caller is None.
    let mut stmt = conn
        .prepare(
            "SELECT r.kind, f.path, r.line, r.col,
                    (SELECT s2.id FROM symbols s2
                       WHERE s2.file_id = r.file_id
                         AND s2.start_line <= r.line
                         AND s2.end_line >= r.line
                       ORDER BY (s2.end_line - s2.start_line) ASC
                       LIMIT 1) AS caller_id
             FROM refs r
             JOIN files f ON f.id = r.file_id
             WHERE r.symbol_id = ?1
             ORDER BY caller_id ASC, r.line ASC
             LIMIT ?2",
        )
        .map_err(|e| LensError::other(format!("refs: prepare non-call sites: {e}")))?;
    let raw: Vec<(String, String, i64, i64, Option<i64>)> = stmt
        .query_map(rusqlite::params![sid, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .map_err(|e| LensError::other(format!("refs: query non-call sites: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("refs: collect non-call sites: {e}")))?;

    let mut out: Vec<RefSite> = Vec::with_capacity(raw.len());
    for (kind, file_path, line, col, caller_id) in raw {
        let caller = match caller_id {
            Some(cid) => load_caller_node(storage, cid)?,
            None => None,
        };
        out.push(RefSite { caller, kind, file_path, line, col });
    }
    Ok(out)
}

fn load_caller_node(storage: &Storage, sid: i64) -> Result<Option<QueryNode>> {
    let conn = storage.connection();
    conn.query_row(
        "SELECT s.qualified_name, s.name, s.kind, f.path, s.start_line
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.id = ?1",
        rusqlite::params![sid],
        |row| {
            Ok(QueryNode {
                symbol_id: sid,
                qualified_name: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                file_path: row.get(3)?,
                start_line: row.get(4)?,
                is_seed: false,
            })
        },
    )
    .optional()
    .map_err(|e| LensError::other(format!("refs: load caller node: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedRef, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn sym(qname: &str, name: &str, start_line: u32, end_line: u32) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: "function".into(),
            start_line,
            start_col: 0,
            end_line,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 0,
            signature: Some(format!("pub fn {name}()")),
            visibility: None,
            parent_qualified_name: None,
            doc_comment: None,
        }
    }

    fn file(path: &str, syms: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Rust);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 100;
        ef.modified_at = 1;
        ef.symbols = syms;
        ef
    }

    fn id_of(storage: &Storage, qname: &str) -> i64 {
        storage
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = ?1",
                rusqlite::params![qname],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn test_refs_returns_none_for_unknown_sid() {
        let (_dir, s) = tmp_storage();
        let r = list_refs(&s, 99999, 100).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_refs_returns_callers_of_a_called_symbol() {
        let (_dir, mut s) = tmp_storage();
        // 3 callers in 3 files all calling `target`.
        let mut files: Vec<ExtractedFile> = Vec::new();
        for i in 0..3 {
            let cn = format!("caller_{i}");
            let mut ef = file(&format!("c{i}.rs"), vec![sym(&cn, &cn, 1, 5)]);
            ef.calls.push(ExtractedCall {
                caller_qualified_name: cn,
                callee_raw_name: "target".into(),
                line: 2,
                col: 4,
            });
            files.push(ef);
        }
        files.push(file("t.rs", vec![sym("target", "target", 1, 3)]));
        insert_extracted_files(&mut s, &files).unwrap();
        resolve_cross_file_references(&mut s).unwrap();

        let sid = id_of(&s, "target");
        let r = list_refs(&s, sid, 100).unwrap().unwrap();
        assert_eq!(r.focus.qualified_name, "target");
        assert_eq!(r.sites.len(), 3, "expected 3 call sites");
        assert!(!r.truncated);
        // All sites are kind=call.
        for site in &r.sites {
            assert_eq!(site.kind, "call");
            assert!(site.caller.is_some());
            assert_eq!(site.line, 2);
        }
        // Ordering: by caller symbol_id ascending.
        let ids: Vec<i64> = r
            .sites
            .iter()
            .map(|s| s.caller.as_ref().unwrap().symbol_id)
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn test_refs_truncates_at_limit_and_flags() {
        let (_dir, mut s) = tmp_storage();
        let mut files: Vec<ExtractedFile> = Vec::new();
        for i in 0..5 {
            let cn = format!("c{i}");
            let mut ef = file(&format!("f{i}.rs"), vec![sym(&cn, &cn, 1, 5)]);
            ef.calls.push(ExtractedCall {
                caller_qualified_name: cn,
                callee_raw_name: "target".into(),
                line: 1,
                col: 0,
            });
            files.push(ef);
        }
        files.push(file("t.rs", vec![sym("target", "target", 1, 3)]));
        insert_extracted_files(&mut s, &files).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let sid = id_of(&s, "target");

        let r = list_refs(&s, sid, 2).unwrap().unwrap();
        assert_eq!(r.sites.len(), 2);
        assert!(r.truncated, "5 callers but limit=2 must set truncated");
    }

    #[test]
    fn test_refs_no_callers_returns_empty_sites_not_none() {
        let (_dir, mut s) = tmp_storage();
        insert_extracted_files(
            &mut s,
            &[file("t.rs", vec![sym("orphan", "orphan", 1, 3)])],
        )
        .unwrap();
        let sid = id_of(&s, "orphan");
        let r = list_refs(&s, sid, 100).unwrap().unwrap();
        assert!(r.sites.is_empty());
        assert!(!r.truncated);
    }

    #[test]
    fn test_refs_includes_non_call_refs_when_no_calls_exhausted() {
        // A `refs` row pointing at the target — e.g. a type mention.
        let (_dir, mut s) = tmp_storage();
        let mut user = file("u.rs", vec![sym("user", "user", 1, 5)]);
        user.refs.push(ExtractedRef {
            raw_name: "target".into(),
            kind: "type".into(),
            line: 3,
            col: 12,
            end_line: 3,
            end_col: 18,
        });
        let target = file("t.rs", vec![sym("target", "target", 1, 3)]);
        insert_extracted_files(&mut s, &[user, target]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let sid = id_of(&s, "target");

        let r = list_refs(&s, sid, 100).unwrap().unwrap();
        // Either a call or a non-call ref; the resolver decides which table
        // claims it. We assert *some* site exists with the expected file/line.
        assert!(!r.sites.is_empty(), "type-mention ref should produce at least one site");
        assert!(r.sites.iter().any(|s| s.file_path == "u.rs" && s.line == 3));
    }

    #[test]
    fn test_refs_clamps_limit_at_hard_limit() {
        // Verify that even an absurd limit doesn't blow up — bounded by HARD_LIMIT.
        let (_dir, mut s) = tmp_storage();
        insert_extracted_files(
            &mut s,
            &[file("t.rs", vec![sym("alone", "alone", 1, 3)])],
        )
        .unwrap();
        let sid = id_of(&s, "alone");
        let r = list_refs(&s, sid, u32::MAX).unwrap().unwrap();
        assert!(r.sites.is_empty());
    }
}
