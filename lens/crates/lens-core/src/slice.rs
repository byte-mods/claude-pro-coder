//! `lens slice <file:line>` — minimal context for a file:line location.
//!
//! Returns the smallest symbol whose `[start_line, end_line]` range contains
//! `line`, plus that symbol's signature, body slice (truncated to fit the
//! token budget), and the same-file imports list. The intent: hand Claude
//! (or a developer) just enough context to understand a single line without
//! dragging in the whole file.
//!
//! Resolution rule: when several symbols enclose `line` (nested fns, methods
//! inside structs), the smallest range wins — that's the innermost lexical
//! scope. Ties (rare) break by ascending `symbol_id` for determinism.

use std::path::Path;

use rusqlite::OptionalExtension;

use crate::error::{LensError, Result};
use crate::follow::CHARS_PER_TOKEN;
use crate::query::QueryNode;
use crate::storage::Storage;

/// Maximum number of imports surfaced in the slice header.
pub const MAX_IMPORTS: usize = 16;

/// What `lens slice <file:line>` returns: the enclosing symbol + its
/// signature + a budget-fitted body slice + same-file imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceResult {
    /// The enclosing symbol — innermost lexical scope at `line`.
    pub focus: QueryNode,
    /// Declaration signature (if recorded for this symbol kind).
    pub signature: Option<String>,
    /// Body lines from the source file. Already truncated to fit the
    /// budget; signature reserved first.
    pub body: Vec<String>,
    /// True when the original body was longer than the budget allowed.
    pub body_truncated: bool,
    /// Up to [`MAX_IMPORTS`] same-file imports (`raw_path` from the
    /// `imports` table), preserved in source order by `line`.
    pub imports: Vec<String>,
}

/// Look up the smallest symbol enclosing `(file_path, line)` and assemble
/// a [`SliceResult`].
///
/// `file_path` must be relative to `root` (matches the `files.path` column
/// shape — the CLI is responsible for normalising user input). `line` is
/// 1-indexed (matches `start_line`/`end_line` in the index).
///
/// Returns `Ok(None)` when:
/// - `file_path` is not indexed under `root`,
/// - no symbol's range encloses `line`.
pub fn slice_at(
    storage: &Storage,
    root: &Path,
    file_path: &str,
    line: u32,
    budget_tokens: u32,
) -> Result<Option<SliceResult>> {
    let conn = storage.connection();

    // 1. Resolve file_path → file_id.
    let file_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE path = ?1",
            rusqlite::params![file_path],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| LensError::other(format!("slice: query file_id: {e}")))?;
    let Some(file_id) = file_id else {
        return Ok(None);
    };

    // 2. Smallest enclosing symbol. Ties broken by symbol_id ascending.
    //    `(end_line - start_line)` may be 0 for single-line symbols; that's
    //    still smaller than any enclosing parent, which is the desired pick.
    let focus_row = conn
        .query_row(
            "SELECT s.id, s.qualified_name, s.name, s.kind, s.start_line, s.end_line,
                    s.body_start_byte, s.body_end_byte, s.signature, f.path
             FROM symbols s
             JOIN files f ON f.id = s.file_id
             WHERE s.file_id = ?1
               AND s.start_line <= ?2
               AND s.end_line >= ?2
             ORDER BY (s.end_line - s.start_line) ASC, s.id ASC
             LIMIT 1",
            rusqlite::params![file_id, line],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|e| LensError::other(format!("slice: query enclosing symbol: {e}")))?;

    let Some((sid, qname, name, kind, start_line, _end_line, body_start, body_end, signature, file_path_out)) =
        focus_row
    else {
        return Ok(None);
    };

    let focus = QueryNode {
        symbol_id: sid,
        qualified_name: qname,
        name,
        kind,
        file_path: file_path_out.clone(),
        start_line,
        is_seed: true,
    };

    // 3. Read body bytes from disk and fit to budget. Best-effort: missing
    //    or post-modified files yield empty body, never a panic.
    let body_text = read_body_slice(root, &file_path_out, body_start, body_end);
    let max_chars = (budget_tokens as usize).saturating_mul(CHARS_PER_TOKEN as usize);
    let sig_chars = signature.as_ref().map(|s| s.len() + 1).unwrap_or(0);
    let remaining = max_chars.saturating_sub(sig_chars);
    let lines: Vec<&str> = body_text.lines().collect();
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut total: usize = 0;
    let mut truncated = false;
    for line_str in &lines {
        let cost = line_str.len() + 1;
        if total.saturating_add(cost) > remaining {
            truncated = true;
            break;
        }
        total = total.saturating_add(cost);
        kept.push((*line_str).to_string());
    }

    // 4. Same-file imports, source order, capped.
    let imports = load_imports_for_file(storage, file_id)?;

    Ok(Some(SliceResult {
        focus,
        signature,
        body: kept,
        body_truncated: truncated,
        imports,
    }))
}

fn read_body_slice(root: &Path, file_path: &str, body_start: i64, body_end: i64) -> String {
    if body_start < 0 || body_end <= body_start {
        return String::new();
    }
    let abs = root.join(file_path);
    let text = match std::fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let start = body_start as usize;
    let end = body_end as usize;
    if end > text.len() {
        return String::new();
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return String::new();
    }
    text[start..end].to_string()
}

fn load_imports_for_file(storage: &Storage, file_id: i64) -> Result<Vec<String>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare(
            "SELECT raw_path FROM imports WHERE file_id = ?1 ORDER BY line ASC, id ASC LIMIT ?2",
        )
        .map_err(|e| LensError::other(format!("slice: prepare imports: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![file_id, MAX_IMPORTS as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| LensError::other(format!("slice: query imports: {e}")))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("slice: collect imports: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedFile, ExtractedImport, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    #[allow(clippy::too_many_arguments)]
    fn sym(
        qname: &str,
        name: &str,
        kind: &str,
        signature: Option<&str>,
        start_line: u32,
        end_line: u32,
        body_start_byte: u32,
        body_end_byte: u32,
    ) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: kind.into(),
            start_line,
            start_col: 0,
            end_line,
            end_col: 0,
            body_start_byte,
            body_end_byte,
            signature: signature.map(str::to_string),
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

    fn write_file(root: &Path, rel: &str, contents: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, contents).unwrap();
    }

    #[test]
    fn test_slice_returns_enclosing_symbol_at_known_line() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        let contents = "pub fn outer() {\n    let x = 1;\n    let y = 2;\n}\n";
        write_file(root, "a.rs", contents);
        let body_start: u32 = 16; // after `pub fn outer() `+ `{`+`\n` ish
        let body_end: u32 = (contents.len() - 2) as u32;
        let f = file(
            "a.rs",
            vec![sym(
                "outer",
                "outer",
                "function",
                Some("pub fn outer()"),
                1,
                4,
                body_start,
                body_end,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();

        // Line 2 is inside outer's body.
        let r = slice_at(&s, root, "a.rs", 2, 5000).unwrap().unwrap();
        assert_eq!(r.focus.qualified_name, "outer");
        assert_eq!(r.signature.as_deref(), Some("pub fn outer()"));
    }

    #[test]
    fn test_slice_returns_smallest_enclosing_when_nested() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // Outer fn 1..10; inner fn 4..6. Line 5 is inside both — inner wins.
        write_file(
            root,
            "a.rs",
            "pub fn outer() {\n    \n    \n    pub fn inner() {\n        let z = 0;\n    }\n    \n    \n    \n}\n",
        );
        let f = file(
            "a.rs",
            vec![
                sym("outer", "outer", "function", Some("pub fn outer()"), 1, 10, 0, 0),
                sym("inner", "inner", "function", Some("pub fn inner()"), 4, 6, 0, 0),
            ],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let r = slice_at(&s, root, "a.rs", 5, 5000).unwrap().unwrap();
        assert_eq!(r.focus.qualified_name, "inner", "smallest range must win");
    }

    #[test]
    fn test_slice_returns_none_when_line_outside_any_symbol() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        write_file(root, "a.rs", "pub fn fn_at_top() {}\n\nlet _ = 1;\n");
        let f = file(
            "a.rs",
            vec![sym("fn_at_top", "fn_at_top", "function", None, 1, 1, 0, 0)],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let r = slice_at(&s, root, "a.rs", 99, 5000).unwrap();
        assert!(r.is_none(), "line 99 not enclosed by any symbol → None");
    }

    #[test]
    fn test_slice_returns_none_when_file_not_indexed() {
        let (dir, s) = tmp_storage();
        let root = dir.path();
        // Empty index — nothing matches.
        let r = slice_at(&s, root, "ghost.rs", 1, 5000).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_slice_truncates_body_when_over_budget() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // 30 lines of body, ~30 chars each = ~900 chars. Budget 50 tokens
        // = 200 chars; signature reserves first → most of body must drop.
        let body: String = (0..30)
            .map(|i| format!("    let var_{i:02} = 12345;"))
            .collect::<Vec<_>>()
            .join("\n");
        let contents = format!("pub fn big() {{\n{body}\n}}\n");
        let pre_len = "pub fn big() ".len() + 1; // up to and including `{`
        write_file(root, "a.rs", &contents);
        let f = file(
            "a.rs",
            vec![sym(
                "big",
                "big",
                "function",
                Some("pub fn big()"),
                1,
                32,
                pre_len as u32,
                (contents.len() - 2) as u32,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        // Line 5 is in the body.
        let r = slice_at(&s, root, "a.rs", 5, 50).unwrap().unwrap();
        assert!(r.body_truncated);
        assert!(r.body.len() < 30, "expected fewer than 30 lines kept");
    }

    #[test]
    fn test_slice_includes_imports_from_same_file() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        write_file(root, "a.rs", "use std::fs;\nuse std::path::Path;\npub fn f() {}\n");
        let mut f = file(
            "a.rs",
            vec![sym("f", "f", "function", Some("pub fn f()"), 3, 3, 0, 0)],
        );
        f.imports.push(ExtractedImport {
            raw_path: "std::fs".into(),
            alias: None,
            line: 1,
        });
        f.imports.push(ExtractedImport {
            raw_path: "std::path::Path".into(),
            alias: None,
            line: 2,
        });
        insert_extracted_files(&mut s, &[f]).unwrap();
        let r = slice_at(&s, root, "a.rs", 3, 5000).unwrap().unwrap();
        assert_eq!(r.imports.len(), 2);
        assert_eq!(r.imports[0], "std::fs", "imports preserved in source order");
        assert_eq!(r.imports[1], "std::path::Path");
    }

    #[test]
    fn test_slice_caps_imports_at_max_imports() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        write_file(root, "a.rs", "pub fn f() {}\n");
        let mut f = file(
            "a.rs",
            vec![sym("f", "f", "function", None, 1, 1, 0, 0)],
        );
        for i in 0..(MAX_IMPORTS + 5) {
            f.imports.push(ExtractedImport {
                raw_path: format!("mod_{i}"),
                alias: None,
                line: (i + 1) as u32,
            });
        }
        insert_extracted_files(&mut s, &[f]).unwrap();
        let r = slice_at(&s, root, "a.rs", 1, 5000).unwrap().unwrap();
        assert_eq!(r.imports.len(), MAX_IMPORTS, "imports must be capped");
    }

    #[test]
    fn test_slice_returns_none_for_line_zero() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        write_file(root, "a.rs", "pub fn f() {}\n");
        let f = file(
            "a.rs",
            vec![sym("f", "f", "function", None, 1, 1, 0, 0)],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        // Line 0 is below all start_lines (which are 1-indexed).
        let r = slice_at(&s, root, "a.rs", 0, 5000).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_slice_handles_missing_source_file_without_error() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // Insert symbol pointing to a file that doesn't exist on disk.
        let f = file(
            "ghost.rs",
            vec![sym(
                "ghost",
                "ghost",
                "function",
                Some("pub fn ghost()"),
                1,
                1,
                0,
                10,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let r = slice_at(&s, root, "ghost.rs", 1, 5000).unwrap().unwrap();
        assert!(r.body.is_empty(), "missing file → empty body, no error");
        assert_eq!(r.signature.as_deref(), Some("pub fn ghost()"));
    }
}
