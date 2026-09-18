//! `lens deps <file>` — how one file is connected to the rest of the project.
//!
//! IDE indexes answer "what does this file import, who imports it, who calls
//! into it" without opening the file. This module answers the same from the
//! symbol graph: import edges (resolved to project files where possible),
//! call edges aggregated per counterpart file, and the file's most-called
//! symbols. It is the file-level complement to `lens follow` (symbol-level)
//! and `lens map` (directory-level), and the cheapest way for a model to
//! decide whether a file is in the blast radius of a change.

use rusqlite::{params, OptionalExtension};

use crate::error::{LensError, Result};
use crate::map::MapSymbol;
use crate::storage::Storage;

/// Top-N symbols listed for the file, ranked by caller count.
pub const TOP_SYMBOLS: usize = 8;

/// One import edge. `resolved_path` is `None` for external packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEdge {
    pub raw_path: String,
    pub resolved_path: Option<String>,
}

/// One aggregated call edge to/from another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {
    pub path: String,
    pub call_sites: i64,
}

/// What `lens deps` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDeps {
    pub path: String,
    pub language: String,
    pub symbol_count: i64,
    /// What this file imports (source order, de-duplicated by raw path).
    pub imports_out: Vec<ImportEdge>,
    /// Files whose imports resolve to this file.
    pub importers_in: Vec<ImportEdge>,
    /// Files this file calls into (by resolved callee), most-called first.
    pub calls_out: Vec<CallEdge>,
    /// Files that call into this file's symbols, most-calling first.
    pub calls_in: Vec<CallEdge>,
    /// This file's symbols ranked by caller count.
    pub top_symbols: Vec<MapSymbol>,
}

/// Build the dependency card for `path` (project-relative, `/`-separated).
/// `Ok(None)` when the file is not in the symbol index.
pub fn file_deps(storage: &Storage, path: &str) -> Result<Option<FileDeps>> {
    let conn = storage.connection();
    let head: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, language FROM files WHERE path = ?1",
            params![path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| LensError::other(format!("deps: query file: {e}")))?;
    let Some((file_id, language)) = head else {
        return Ok(None);
    };

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE file_id = ?1", params![file_id], |r| r.get(0))
        .map_err(|e| LensError::other(format!("deps: count symbols: {e}")))?;

    let imports_out = {
        let mut stmt = conn
            .prepare(
                "SELECT i.raw_path, f2.path FROM imports i
                 LEFT JOIN files f2 ON f2.id = i.resolved_file_id
                 WHERE i.file_id = ?1 ORDER BY i.line ASC, i.id ASC",
            )
            .map_err(|e| LensError::other(format!("deps: prepare imports out: {e}")))?;
        let rows = stmt
            .query_map(params![file_id], |r| {
                Ok(ImportEdge { raw_path: r.get(0)?, resolved_path: r.get(1)? })
            })
            .map_err(|e| LensError::other(format!("deps: query imports out: {e}")))?;
        let mut out: Vec<ImportEdge> = Vec::new();
        for r in rows {
            let e = r.map_err(|e| LensError::other(format!("deps: row imports out: {e}")))?;
            if !out.iter().any(|x| x.raw_path == e.raw_path) {
                out.push(e);
            }
        }
        out
    };

    let importers_in = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT f1.path, i.raw_path FROM imports i
                 JOIN files f1 ON f1.id = i.file_id
                 WHERE i.resolved_file_id = ?1 AND i.file_id != ?1
                 ORDER BY f1.path ASC, i.raw_path ASC",
            )
            .map_err(|e| LensError::other(format!("deps: prepare importers: {e}")))?;
        let rows = stmt
            .query_map(params![file_id], |r| {
                Ok(ImportEdge { raw_path: r.get(1)?, resolved_path: Some(r.get(0)?) })
            })
            .map_err(|e| LensError::other(format!("deps: query importers: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LensError::other(format!("deps: collect importers: {e}")))?
    };

    let calls_out = call_edges(
        storage,
        "SELECT f2.path, COUNT(*) FROM calls c
         JOIN symbols s2 ON s2.id = c.callee_symbol_id
         JOIN files f2 ON f2.id = s2.file_id
         WHERE c.file_id = ?1 AND f2.id != ?1
         GROUP BY f2.path ORDER BY COUNT(*) DESC, f2.path ASC",
        file_id,
    )?;
    let calls_in = call_edges(
        storage,
        "SELECT f1.path, COUNT(*) FROM calls c
         JOIN symbols s2 ON s2.id = c.callee_symbol_id
         JOIN files f1 ON f1.id = c.file_id
         WHERE s2.file_id = ?1 AND c.file_id != ?1
         GROUP BY f1.path ORDER BY COUNT(*) DESC, f1.path ASC",
        file_id,
    )?;

    let top_symbols = {
        let mut stmt = conn
            .prepare(
                "SELECT s.qualified_name, s.kind, f.path, s.start_line,
                        COALESCE((SELECT COUNT(*) FROM calls c WHERE c.callee_symbol_id = s.id), 0) AS callers
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE s.file_id = ?1
                 ORDER BY callers DESC, s.qualified_name ASC
                 LIMIT ?2",
            )
            .map_err(|e| LensError::other(format!("deps: prepare top symbols: {e}")))?;
        let rows = stmt
            .query_map(params![file_id, TOP_SYMBOLS as i64], |r| {
                Ok(MapSymbol {
                    qualified_name: r.get(0)?,
                    kind: r.get(1)?,
                    file_path: r.get(2)?,
                    start_line: r.get(3)?,
                    caller_count: r.get(4)?,
                })
            })
            .map_err(|e| LensError::other(format!("deps: query top symbols: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LensError::other(format!("deps: collect top symbols: {e}")))?
    };

    Ok(Some(FileDeps {
        path: path.to_string(),
        language,
        symbol_count,
        imports_out,
        importers_in,
        calls_out,
        calls_in,
        top_symbols,
    }))
}

fn call_edges(storage: &Storage, sql: &str, file_id: i64) -> Result<Vec<CallEdge>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| LensError::other(format!("deps: prepare call edges: {e}")))?;
    let rows = stmt
        .query_map(params![file_id], |r| Ok(CallEdge { path: r.get(0)?, call_sites: r.get(1)? }))
        .map_err(|e| LensError::other(format!("deps: query call edges: {e}")))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("deps: collect call edges: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedImport, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        (dir, Storage::open(&path).unwrap())
    }

    fn sym(qname: &str, name: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: "function".into(),
            start_line: 1,
            start_col: 0,
            end_line: 2,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 0,
            signature: None,
            visibility: None,
            parent_qualified_name: None,
            doc_comment: None,
        }
    }

    fn py(path: &str, syms: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Python);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 1;
        ef.modified_at = 1;
        ef.symbols = syms;
        ef
    }

    fn fixture() -> (TempDir, Storage) {
        let (g, mut s) = tmp_storage();
        let util = py("pkg/util.py", vec![sym("pkg.util.helper", "helper"), sym("pkg.util.other", "other")]);
        let mut app = py("pkg/app.py", vec![sym("pkg.app.main", "main")]);
        app.imports.push(ExtractedImport { raw_path: "pkg.util.helper".into(), alias: None, line: 1 });
        app.imports.push(ExtractedImport { raw_path: "os".into(), alias: None, line: 2 });
        app.calls.push(ExtractedCall { caller_qualified_name: "pkg.app.main".into(), callee_raw_name: "helper".into(), line: 3, col: 0 });
        app.calls.push(ExtractedCall { caller_qualified_name: "pkg.app.main".into(), callee_raw_name: "helper".into(), line: 4, col: 0 });
        insert_extracted_files(&mut s, &[util, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        (g, s)
    }

    #[test]
    fn test_deps_returns_none_for_unindexed_file() {
        let (_g, s) = tmp_storage();
        assert!(file_deps(&s, "ghost.py").unwrap().is_none());
    }

    #[test]
    fn test_deps_reports_imports_in_both_directions() {
        let (_g, s) = fixture();
        let app = file_deps(&s, "pkg/app.py").unwrap().unwrap();
        assert_eq!(app.language, "python");
        assert_eq!(app.symbol_count, 1);
        assert_eq!(app.imports_out.len(), 2);
        assert_eq!(app.imports_out[0].raw_path, "pkg.util.helper");
        assert_eq!(app.imports_out[0].resolved_path.as_deref(), Some("pkg/util.py"));
        assert_eq!(app.imports_out[1].resolved_path, None, "external import stays unresolved");
        assert!(app.importers_in.is_empty());

        let util = file_deps(&s, "pkg/util.py").unwrap().unwrap();
        assert_eq!(util.importers_in.len(), 1);
        assert_eq!(util.importers_in[0].resolved_path.as_deref(), Some("pkg/app.py"));
    }

    #[test]
    fn test_deps_aggregates_call_edges_per_file_and_ranks_symbols() {
        let (_g, s) = fixture();
        let app = file_deps(&s, "pkg/app.py").unwrap().unwrap();
        assert_eq!(app.calls_out, vec![CallEdge { path: "pkg/util.py".into(), call_sites: 2 }]);
        assert!(app.calls_in.is_empty());

        let util = file_deps(&s, "pkg/util.py").unwrap().unwrap();
        assert_eq!(util.calls_in, vec![CallEdge { path: "pkg/app.py".into(), call_sites: 2 }]);
        assert_eq!(util.top_symbols[0].qualified_name, "pkg.util.helper");
        assert_eq!(util.top_symbols[0].caller_count, 2);
        assert_eq!(util.top_symbols[1].caller_count, 0);
    }
}
