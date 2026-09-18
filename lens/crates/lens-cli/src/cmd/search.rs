//! `lens search <query>` — keyword search over every text file in the
//! project (any language, plus markdown/config and the agent's own state
//! notes), returning `file:line` hits with the enclosing symbol when known.
//!
//! This is the budget-capped replacement for a `Grep` over the tree: the
//! index is FTS5, ranking is BM25 per file, and output stops at `--budget`
//! tokens instead of flooding the context with every match.

use std::path::Path;

use lens_core::{search_docs, SearchOptions, SearchResult};

pub fn run(
    query: &str,
    budget: u32,
    limit: u32,
    scope: Option<&Path>,
    kind: Option<&str>,
) -> Result<(), u8> {
    let cwd = match crate::cmd::util::cwd_project_root("search") {
        Ok(p) => p,
        Err(code) => return Err(code),
    };
    run_with_root(&cwd, query, budget, limit, scope, kind)
}

pub fn run_with_root(
    root: &Path,
    query: &str,
    budget: u32,
    limit: u32,
    scope: Option<&Path>,
    kind: Option<&str>,
) -> Result<(), u8> {
    if query.trim().is_empty() {
        eprintln!("lens search: query must not be empty.");
        return Err(2);
    }
    if let Some(k) = kind {
        if !lens_core::ALL_KINDS.contains(&k) {
            eprintln!("lens search: --kind must be one of {} (got '{k}').", lens_core::ALL_KINDS.join(", "));
            return Err(2);
        }
    }
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "search") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let scope_rel = match scope {
        Some(p) => match lens_core::docs::scope_to_relative(root, p) {
            Some(s) => Some(s),
            None => {
                eprintln!(
                    "lens search: --scope '{}' is not under project root '{}'",
                    p.display(),
                    root.display()
                );
                return Err(1);
            }
        },
        None => None,
    };
    let opts = SearchOptions { limit, budget, scope: scope_rel, kind: kind.map(str::to_string) };
    let result = match search_docs(&storage, query, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lens search: query failed: {e}");
            return Err(1);
        }
    };
    let touched: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    let rendered = render_markdown(&result);
    crate::cmd::util::finish(root, &storage, rendered, &touched);
    Ok(())
}

/// Pure markdown formatter — no fs / no Storage.
pub fn render_markdown(result: &SearchResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Search: `{}`", result.query);
    let _ = writeln!(&mut out);
    if result.terms.is_empty() {
        let _ = writeln!(&mut out, "_Empty query after tokenisation — nothing to search for._");
        return out;
    }
    if result.files.is_empty() {
        let _ = writeln!(
            &mut out,
            "_No files matched `{}`. Terms are prefix-matched per word; try a shorter stem or a different word._",
            result.terms.join(" ")
        );
        return out;
    }
    let hits: usize = result.files.iter().map(|f| f.hits.len()).sum();
    let _ = writeln!(
        &mut out,
        "**Terms:** {} • **Files matched:** {} (showing {}) • **Hits shown:** {}{}",
        result.terms.iter().map(|t| format!("`{t}*`")).collect::<Vec<_>>().join(", "),
        result.files_matched,
        result.files.len(),
        hits,
        if result.truncated { " • _truncated — raise --budget / --limit or narrow --scope_" } else { "" }
    );
    if result.relaxed {
        let _ = writeln!(&mut out, "_No file contains every term; showing files matching any term, best first. Use `AND` to force all._");
    }
    let _ = writeln!(&mut out);
    for f in &result.files {
        let _ = writeln!(
            &mut out,
            "## `{}` ({}, {} matching line{})",
            f.path,
            f.kind,
            f.matching_lines,
            if f.matching_lines == 1 { "" } else { "s" }
        );
        for h in &f.hits {
            match &h.symbol {
                Some((qname, kind, _)) => {
                    let _ = writeln!(&mut out, "- `{}:{}` in `{qname}` ({kind}): `{}`", f.path, h.line, h.text);
                }
                None => {
                    let _ = writeln!(&mut out, "- `{}:{}`: `{}`", f.path, h.line, h.text);
                }
            }
        }
        let _ = writeln!(&mut out);
    }
    let _ = writeln!(
        &mut out,
        "_search: budget {} tokens, est {} tokens_",
        result.budget, result.estimated_tokens
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::{SearchFile, SearchHit};
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    #[test]
    fn test_search_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_with_root(dir.path(), "x", 2000, 20, None, None), Err(1));
    }

    #[test]
    fn test_search_run_rejects_empty_query_and_bad_kind() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_with_root(dir.path(), "   ", 2000, 20, None, None), Err(2));
        assert_eq!(run_with_root(dir.path(), "x", 2000, 20, None, Some("blob")), Err(2));
    }

    #[test]
    fn test_search_run_finds_text_in_unsupported_language_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("main.rb"), "def greet\n  puts 'hello'\nend\n");
        write(&root.join("src/a.rs"), "pub fn a() {}\n");
        crate::cmd::index::run(Some(root)).unwrap();
        assert_eq!(run_with_root(root, "greet", 2000, 20, None, None), Ok(()));
        assert_eq!(run_with_root(root, "greet", 2000, 20, Some(Path::new("src")), None), Ok(()));
    }

    #[test]
    fn test_render_markdown_lists_files_and_hits_with_symbols() {
        let r = SearchResult {
            query: "ensure".into(),
            terms: vec!["ensure".into()],
            files: vec![SearchFile {
                path: "src/f.rs".into(),
                kind: "code".into(),
                matching_lines: 2,
                hits: vec![
                    SearchHit { line: 3, text: "pub fn ensure_fresh() {".into(), symbol: Some(("f::ensure_fresh".into(), "function".into(), 3)) },
                    SearchHit { line: 9, text: "// ensure".into(), symbol: None },
                ],
            }],
            files_matched: 1,
            truncated: false,
            relaxed: false,
            budget: 2000,
            estimated_tokens: 40,
        };
        let md = render_markdown(&r);
        assert!(md.contains("# Search: `ensure`"));
        assert!(md.contains("## `src/f.rs` (code, 2 matching lines)"));
        assert!(md.contains("`src/f.rs:3` in `f::ensure_fresh` (function)"));
        assert!(md.contains("`src/f.rs:9`: `// ensure`"));
        assert!(md.contains("est 40 tokens"));
    }

    #[test]
    fn test_render_markdown_no_match_and_truncation_notes() {
        let empty = SearchResult { query: "zz".into(), terms: vec!["zz".into()], ..Default::default() };
        assert!(render_markdown(&empty).contains("No files matched"));
        let trunc = SearchResult {
            query: "a".into(),
            terms: vec!["a".into()],
            files: vec![SearchFile { path: "x".into(), kind: "text".into(), hits: vec![], matching_lines: 0 }],
            files_matched: 30,
            truncated: true,
            relaxed: true,
            budget: 100,
            estimated_tokens: 100,
        };
        assert!(render_markdown(&trunc).contains("matching any term"));
        assert!(render_markdown(&trunc).contains("truncated"));
    }
}
