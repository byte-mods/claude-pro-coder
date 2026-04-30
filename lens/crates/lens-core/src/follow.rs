//! `lens follow <symbol>` — Ctrl+Click. Definition site + minimal slice of
//! the body that fits a token budget, plus a deterministic top-N caller list.
//!
//! Designed so Claude (or a developer) can pull *just enough* context to
//! understand a symbol without dragging in whole files. This is the headline
//! token-saving primitive in lens.
//!
//! Resolution from a free-form symbol string is the caller's job (use
//! [`crate::resolve_symbol_to_id`] over a [`crate::Graph`]); this module
//! takes an already-resolved `symbol_id`.

use std::path::Path;

use rusqlite::OptionalExtension;

use crate::error::{LensError, Result};
use crate::query::QueryNode;
use crate::storage::Storage;

/// Approximate char-per-token ratio used to translate a token budget into a
/// char budget when truncating body text. Conservative — most code tokenises
/// into shorter pieces than English prose.
pub const CHARS_PER_TOKEN: u32 = 4;

/// Maximum number of callers returned, deterministic by smallest
/// `caller_symbol_id` ascending. Three is enough to convey "who depends on
/// this" without ballooning the output.
pub const MAX_CALLERS: usize = 3;

/// What `lens follow <symbol>` returns: focus location, signature, body slice
/// (already truncated to fit budget), plus the nearest few callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowResult {
    /// The symbol being followed.
    pub focus: QueryNode,
    /// Doc comment harvested at index time. Surfaced before signature/body
    /// so Claude can read author intent without dragging in the full body.
    /// `None` when no doc was attached at the declaration.
    pub doc_comment: Option<String>,
    /// Language of the source file ("rust" / "python" / "typescript" /
    /// "javascript" / "go" / "dart"). Used to surface cross-language matches —
    /// e.g. when `lens follow Foo` resolves to a Python class because the
    /// caller-context Rust type was missing.
    pub language: String,
    /// Declaration signature (e.g. `pub fn foo(x: i32) -> Result<()>`).
    /// `None` for symbol kinds without a stored signature.
    pub signature: Option<String>,
    /// Body lines from the source file. Already truncated to fit the
    /// budget; the signature is always preserved before any body lines.
    pub body: Vec<String>,
    /// True when the original body was longer than the budget allowed.
    pub body_truncated: bool,
    /// Up to [`MAX_CALLERS`] callers, ordered by `symbol_id` ascending.
    pub callers: Vec<QueryNode>,
}

/// Look up a symbol by id and assemble a [`FollowResult`].
///
/// `root` is the project root used to resolve relative file paths from the
/// index — typically the same path passed to `lens index`. `budget_tokens`
/// is the soft cap on output size (signature is always included; body
/// lines are dropped tail-first to fit).
///
/// Returns `Ok(None)` when `sid` is not present in storage.
///
/// # Errors
/// - SQLite read failures (corrupt index, missing file).
/// - Source-file read failures (file referenced by the index has been
///   deleted since indexing). Body bytes are best-effort: if the bytes
///   recorded in the index no longer fall on UTF-8 boundaries (file
///   modified post-index), the body is returned empty rather than panic.
pub fn follow_symbol(
    storage: &Storage,
    root: &Path,
    sid: i64,
    budget_tokens: u32,
) -> Result<Option<FollowResult>> {
    let conn = storage.connection();

    // 1. Focus row + file path + language. Single query joining symbols + files.
    let focus_row = conn
        .query_row(
            "SELECT s.qualified_name, s.name, s.kind, s.start_line, s.end_line,
                    s.body_start_byte, s.body_end_byte, s.signature, f.path,
                    s.doc_comment, f.language
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
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()
        .map_err(|e| LensError::other(format!("follow: query focus row: {e}")))?;

    let Some((qname, name, kind, start_line, _end_line, body_start, body_end, signature, file_path, doc_comment, language)) =
        focus_row
    else {
        return Ok(None);
    };

    let focus = QueryNode {
        symbol_id: sid,
        qualified_name: qname,
        name,
        kind,
        file_path: file_path.clone(),
        start_line,
        is_seed: true,
    };

    // 2. Read source file and extract the body byte range.
    //    Best-effort: missing/short files yield an empty body rather than
    //    propagating a hard error — the focus + signature + callers are
    //    still useful even without body text.
    let body_text = read_body_slice(root, &file_path, body_start, body_end);

    // 3. Fit body to char budget (token budget * CHARS_PER_TOKEN), reserving
    //    space for the signature. Drop body lines tail-first.
    let max_chars = (budget_tokens as usize).saturating_mul(CHARS_PER_TOKEN as usize);
    let sig_chars = signature.as_ref().map(|s| s.len() + 1).unwrap_or(0); // +1 for newline.
    let remaining = max_chars.saturating_sub(sig_chars);
    let lines: Vec<&str> = body_text.lines().collect();
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    let mut total: usize = 0;
    let mut truncated = false;
    for line in &lines {
        let cost = line.len() + 1; // +1 for the newline we conceptually rejoin with.
        if total.saturating_add(cost) > remaining {
            truncated = true;
            break;
        }
        total = total.saturating_add(cost);
        kept.push((*line).to_string());
    }
    if !truncated && kept.len() < lines.len() {
        // Shouldn't normally happen, but covers an edge where iteration
        // exits without setting the flag.
        truncated = true;
    }

    // 4. Top callers, deterministic by smallest symbol_id ascending.
    //    GROUP BY collapses multiple call rows from the same caller to a
    //    single result — `lens follow` answers "who calls me," not "how many
    //    times each caller calls me."
    let callers = load_callers(storage, sid)?;

    Ok(Some(FollowResult {
        focus,
        doc_comment,
        language,
        signature,
        body: kept,
        body_truncated: truncated,
        callers,
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

fn load_callers(storage: &Storage, sid: i64) -> Result<Vec<QueryNode>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.qualified_name, s.name, s.kind, f.path, s.start_line
             FROM calls c
             JOIN symbols s ON s.id = c.caller_symbol_id
             JOIN files f ON f.id = s.file_id
             WHERE c.callee_symbol_id = ?1
             GROUP BY s.id
             ORDER BY s.id ASC
             LIMIT ?2",
        )
        .map_err(|e| LensError::other(format!("follow: prepare callers: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![sid, MAX_CALLERS as i64], |row| {
            Ok(QueryNode {
                symbol_id: row.get(0)?,
                qualified_name: row.get(1)?,
                name: row.get(2)?,
                kind: row.get(3)?,
                file_path: row.get(4)?,
                start_line: row.get(5)?,
                is_seed: false,
            })
        })
        .map_err(|e| LensError::other(format!("follow: query callers: {e}")))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("follow: collect callers: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build an isolated `(TempDir, Storage)` for each test. The TempDir
    /// owns the on-disk lifetime; dropping it at end-of-test cleans up.
    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    /// Construct a synthetic `ExtractedSymbol` with explicit body byte
    /// range. Tests use this to control what body slice gets returned.
    #[allow(clippy::too_many_arguments)]
    fn sym_with_body(
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

    /// Write a source file with a fn body whose byte range is known.
    /// Returns `(body_start_byte, body_end_byte)` of the `{...}` content.
    fn write_fn_with_body(root: &std::path::Path, file: &str, body: &str) -> (u32, u32) {
        let pre = "pub fn target() ";
        let contents = format!("{pre}{{\n{body}\n}}\n");
        let p = root.join(file);
        fs::write(&p, &contents).unwrap();
        // Body is everything between the `{` and matching `}` exclusive.
        let body_start = (pre.len() + 1) as u32; // skip past `{`
        let body_end = (contents.len() - 2) as u32; // before final `}\n`
        (body_start, body_end)
    }

    #[test]
    fn test_follow_returns_definition_with_body_under_budget() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        let (bs, be) = write_fn_with_body(root, "a.rs", "let x = 1;\n    let y = 2;");
        let f = file(
            "a.rs",
            vec![sym_with_body(
                "target",
                "target",
                "function",
                Some("pub fn target()"),
                1,
                4,
                bs,
                be,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let sid = id_of(&s, "target");

        let r = follow_symbol(&s, root, sid, 5000).unwrap().unwrap();
        assert_eq!(r.focus.qualified_name, "target");
        assert_eq!(r.signature.as_deref(), Some("pub fn target()"));
        assert!(!r.body_truncated);
        assert!(
            r.body.iter().any(|l| l.contains("let x = 1;")),
            "body should contain first line; got {:?}",
            r.body
        );
        assert!(r.body.iter().any(|l| l.contains("let y = 2;")));
        assert!(r.callers.is_empty());
    }

    #[test]
    fn test_follow_returns_none_for_unknown_sid() {
        let (dir, s) = tmp_storage();
        let r = follow_symbol(&s, dir.path(), 99999, 1000).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_follow_truncates_body_when_over_budget() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // Body = 20 lines of ~30 chars each = ~600 chars. Budget of 50
        // tokens = 200 chars, so most of the body must be dropped.
        let body: String = (0..20)
            .map(|i| format!("    let var_{i:02} = {i:02} + {i:02};"))
            .collect::<Vec<_>>()
            .join("\n");
        let (bs, be) = write_fn_with_body(root, "a.rs", &body);
        let f = file(
            "a.rs",
            vec![sym_with_body(
                "big",
                "big",
                "function",
                Some("pub fn big()"),
                1,
                25,
                bs,
                be,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let sid = id_of(&s, "big");

        let r = follow_symbol(&s, root, sid, 50).unwrap().unwrap();
        assert!(r.body_truncated, "expected body to be truncated under tight budget");
        assert!(r.body.len() < 20, "expected fewer than 20 body lines; got {}", r.body.len());
        assert_eq!(r.signature.as_deref(), Some("pub fn big()"));
    }

    #[test]
    fn test_follow_signature_always_preserved_even_under_zero_budget() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        let (bs, be) = write_fn_with_body(root, "a.rs", "let x = 1;");
        let f = file(
            "a.rs",
            vec![sym_with_body(
                "tiny",
                "tiny",
                "function",
                Some("pub fn tiny()"),
                1,
                3,
                bs,
                be,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let sid = id_of(&s, "tiny");

        // Budget = 0 → no chars for body, but signature must survive in result.
        let r = follow_symbol(&s, root, sid, 0).unwrap().unwrap();
        assert_eq!(r.signature.as_deref(), Some("pub fn tiny()"));
        assert!(r.body.is_empty(), "no body lines fit under zero budget");
        assert!(r.body_truncated, "truncation flag should be set when body was non-empty pre-fit");
    }

    #[test]
    fn test_follow_returns_callers_capped_at_max_and_ordered() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // 5 callers across 5 files, all calling `target`.
        let mut files: Vec<ExtractedFile> = Vec::new();
        for i in 0..5 {
            let caller_qname = format!("caller_{i}");
            let caller_file = format!("c{i}.rs");
            // Each caller file has its own caller symbol that calls target.
            let mut ef = file(
                &caller_file,
                vec![sym_with_body(
                    &caller_qname,
                    &caller_qname,
                    "function",
                    Some("pub fn caller()"),
                    1,
                    1,
                    0,
                    0,
                )],
            );
            ef.calls.push(ExtractedCall {
                caller_qualified_name: caller_qname,
                callee_raw_name: "target".into(),
                line: 1,
                col: 0,
            });
            files.push(ef);
        }
        // Target file.
        let (bs, be) = write_fn_with_body(root, "target.rs", "let _ = ();");
        files.push(file(
            "target.rs",
            vec![sym_with_body(
                "target",
                "target",
                "function",
                Some("pub fn target()"),
                1,
                3,
                bs,
                be,
            )],
        ));
        insert_extracted_files(&mut s, &files).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let sid = id_of(&s, "target");

        let r = follow_symbol(&s, root, sid, 5000).unwrap().unwrap();
        assert_eq!(r.callers.len(), MAX_CALLERS, "expected exactly MAX_CALLERS callers");
        // Determinism: ascending symbol_id.
        let ids: Vec<i64> = r.callers.iter().map(|c| c.symbol_id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "callers should be sorted ascending by symbol_id");
        // Same caller appearing in multiple call rows must collapse to one row;
        // we have 5 distinct callers so just verify the cap.
    }

    #[test]
    fn test_follow_returns_empty_body_when_source_file_missing() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // Don't write the source file — body byte range will not resolve.
        let f = file(
            "ghost.rs",
            vec![sym_with_body(
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
        let sid = id_of(&s, "ghost");

        let r = follow_symbol(&s, root, sid, 5000).unwrap().unwrap();
        assert!(r.body.is_empty(), "missing source must yield empty body, not error");
        assert_eq!(r.signature.as_deref(), Some("pub fn ghost()"));
    }

    #[test]
    fn test_follow_handles_zero_byte_body_without_panic() {
        let (dir, mut s) = tmp_storage();
        let root = dir.path();
        // body_start == body_end == 0 (declaration-only, no body).
        fs::write(root.join("a.rs"), "pub fn decl();\n").unwrap();
        let f = file(
            "a.rs",
            vec![sym_with_body(
                "decl",
                "decl",
                "function",
                Some("pub fn decl()"),
                1,
                1,
                0,
                0,
            )],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let sid = id_of(&s, "decl");

        let r = follow_symbol(&s, root, sid, 1000).unwrap().unwrap();
        assert!(r.body.is_empty());
        assert!(!r.body_truncated, "no body to truncate");
    }
}
