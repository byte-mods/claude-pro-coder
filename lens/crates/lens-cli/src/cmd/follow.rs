//! `lens follow <symbol>` — Ctrl+Click. Resolve a free-form symbol string,
//! pull the focus + signature + budget-fitted body slice + nearest callers,
//! render a clean markdown card.

use std::path::Path;

use lens_core::{
    follow_symbol, resolve_symbol_to_id, FollowResult, Graph,
};

pub fn run(symbol: &str, from: Option<&str>, budget: u32) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens follow: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, symbol, from, budget)
}

pub fn run_with_root(
    root: &Path,
    symbol: &str,
    from: Option<&str>,
    budget: u32,
) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "follow") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let graph = match Graph::load(&storage) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lens follow: failed to load graph: {e}");
            return Err(1);
        }
    };

    let mut ids = resolve_symbol_to_id(&graph, symbol);
    if ids.is_empty() {
        eprintln!("lens follow: no symbol matched '{symbol}'.");
        return Err(1);
    }

    // Disambiguate via --from FILE:LINE when provided.
    if let Some(from_str) = from {
        if let Some((file, line)) = parse_from(from_str) {
            ids = narrow_by_origin(&graph, &ids, file, line);
            if ids.is_empty() {
                eprintln!(
                    "lens follow: --from '{from_str}' did not match any candidate for '{symbol}'.",
                );
                return Err(1);
            }
        } else {
            eprintln!(
                "lens follow: --from must be of the form FILE:LINE (got '{from_str}'); ignoring.",
            );
        }
    }

    if ids.len() > 1 {
        // Detect cross-language collisions so Claude can tell when an
        // ambiguity stems from the same name living in two different
        // languages (common with utility names like `New`, `Config`,
        // `Server`).
        let langs: std::collections::BTreeSet<&str> = ids
            .iter()
            .filter_map(|sid| graph.symbols.get(sid).map(|m| m.language.as_str()))
            .collect();
        let cross_lang_note = if langs.len() > 1 {
            format!(" cross-language: {}", langs.iter().copied().collect::<Vec<_>>().join(", "))
        } else {
            String::new()
        };
        eprintln!(
            "lens follow: '{symbol}' is ambiguous ({} candidates;{cross_lang_note}). Disambiguate with --from FILE:LINE or a qualified name:",
            ids.len()
        );
        for sid in ids.iter().take(10) {
            if let Some(meta) = graph.symbols.get(sid) {
                eprintln!(
                    "  - [{}] {} ({} at {}:{})",
                    meta.language, meta.qualified_name, meta.kind, meta.file_path, meta.start_line
                );
            }
        }
        return Err(1);
    }

    let sid = ids[0];
    let result = match follow_symbol(&storage, root, sid, budget) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "lens follow: symbol '{symbol}' resolved to id {sid} but storage lookup returned None",
            );
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens follow: lookup failed: {e}");
            return Err(1);
        }
    };

    print!("{}", render_markdown(symbol, &result));
    Ok(())
}

/// Pure markdown formatter — no fs / no Storage / no network. Safe to unit-test
/// in isolation per the markdown-formatter-purity convention.
pub fn render_markdown(input: &str, result: &FollowResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let f = &result.focus;

    let _ = writeln!(&mut out, "# Follow: `{}`", f.qualified_name);
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Kind:** {} • **Language:** {} • **File:** `{}:{}` • **Resolved from:** `{}`",
        f.kind, result.language, f.file_path, f.start_line, input
    );
    let _ = writeln!(&mut out);

    // Doc comment first — author intent is the highest-leverage context.
    // Surfacing it ahead of the signature/body lets Claude often skip
    // reading the body entirely.
    if let Some(doc) = &result.doc_comment {
        let _ = writeln!(&mut out, "**Doc**");
        let _ = writeln!(&mut out);
        for line in doc.lines() {
            let _ = writeln!(&mut out, "> {line}");
        }
        let _ = writeln!(&mut out);
    }

    if let Some(sig) = &result.signature {
        let _ = writeln!(&mut out, "**Signature**");
        let _ = writeln!(&mut out);
        let _ = writeln!(&mut out, "```{}", code_fence_lang(&f.file_path));
        let _ = writeln!(&mut out, "{sig}");
        let _ = writeln!(&mut out, "```");
        let _ = writeln!(&mut out);
    }

    if !result.body.is_empty() {
        if result.body_truncated {
            let _ = writeln!(&mut out, "**Body** ({} lines, truncated to fit budget)", result.body.len());
        } else {
            let _ = writeln!(&mut out, "**Body** ({} lines)", result.body.len());
        }
        let _ = writeln!(&mut out);
        let _ = writeln!(&mut out, "```{}", code_fence_lang(&f.file_path));
        for line in &result.body {
            let _ = writeln!(&mut out, "{line}");
        }
        let _ = writeln!(&mut out, "```");
        let _ = writeln!(&mut out);
    } else if result.body_truncated {
        let _ = writeln!(
            &mut out,
            "_Body omitted — budget too tight for any body lines._"
        );
        let _ = writeln!(&mut out);
    }

    if !result.callers.is_empty() {
        let _ = writeln!(
            &mut out,
            "**Callers** ({} shown)",
            result.callers.len()
        );
        for c in &result.callers {
            let _ = writeln!(
                &mut out,
                "- `{}` ({}) — `{}:{}`",
                c.qualified_name, c.kind, c.file_path, c.start_line
            );
        }
        let _ = writeln!(&mut out);
    } else {
        let _ = writeln!(&mut out, "_No callers recorded in the index._");
        let _ = writeln!(&mut out);
    }
    out
}

fn parse_from(s: &str) -> Option<(&str, u32)> {
    // Split from the rightmost ':' so paths containing ':' (e.g. Windows
    // drive letters) still parse the line-number tail correctly.
    let (file, line) = s.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((file, line))
}

fn narrow_by_origin(graph: &Graph, ids: &[i64], file: &str, _line: u32) -> Vec<i64> {
    // Match by file_path equality; line is informational for the user but
    // not used for narrowing in v1 — symbol start_line may not align with
    // the call-site line they typed.
    ids.iter()
        .copied()
        .filter(|sid| {
            graph
                .symbols
                .get(sid)
                .is_some_and(|m| m.file_path == file)
        })
        .collect()
}

fn code_fence_lang(file_path: &str) -> &'static str {
    match Path::new(file_path).extension().and_then(|s| s.to_str()) {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("ts") | Some("tsx") => "typescript",
        Some("js") | Some("jsx") => "javascript",
        Some("go") => "go",
        Some("java") => "java",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("cc") | Some("hpp") => "cpp",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::QueryNode;
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    fn build_initial_index(root: &Path) {
        crate::cmd::index::run(Some(root)).expect("initial index");
    }

    fn mk_node(id: i64, qname: &str, file: &str, line: i64, kind: &str) -> QueryNode {
        QueryNode {
            symbol_id: id,
            qualified_name: qname.into(),
            name: qname.rsplit("::").next().unwrap_or(qname).into(),
            kind: kind.into(),
            file_path: file.into(),
            start_line: line,
            is_seed: false,
        }
    }

    #[test]
    fn test_follow_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "anything", None, 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_follow_run_errors_on_unknown_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn known() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "ghost", None, 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_follow_run_succeeds_for_known_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn alone() { let _ = 1; }\n");
        build_initial_index(root);
        let r = run_with_root(root, "alone", None, 5000);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_follow_run_disambiguates_via_from_file_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn helper() {}\n");
        write(&root.join("b.rs"), "pub fn helper() {}\n");
        build_initial_index(root);
        // Without --from, helper is ambiguous → error.
        assert_eq!(run_with_root(root, "helper", None, 1000), Err(1));
        // With --from a.rs:1 → unambiguous.
        assert_eq!(run_with_root(root, "helper", Some("a.rs:1"), 1000), Ok(()));
        // With --from b.rs:1 → also unambiguous (different file).
        assert_eq!(run_with_root(root, "helper", Some("b.rs:1"), 1000), Ok(()));
    }

    #[test]
    fn test_follow_run_errors_when_from_matches_no_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn helper() {}\n");
        build_initial_index(root);
        // --from points at a file that doesn't host helper → error.
        let r = run_with_root(root, "helper", Some("nonexistent.rs:1"), 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_render_markdown_minimal_no_signature_no_body_no_callers() {
        let r = FollowResult {
            focus: mk_node(1, "x", "a.rs", 1, "function"),
            doc_comment: None,
            language: "rust".into(),
            signature: None,
            body: vec![],
            body_truncated: false,
            callers: vec![],
        };
        let md = render_markdown("x", &r);
        assert!(md.contains("# Follow: `x`"));
        assert!(md.contains("**Kind:** function"));
        assert!(md.contains("**Resolved from:** `x`"));
        assert!(md.contains("No callers recorded"));
        assert!(!md.contains("**Signature**"));
        assert!(!md.contains("**Body**"));
    }

    #[test]
    fn test_render_markdown_with_signature_and_body() {
        let r = FollowResult {
            focus: mk_node(1, "foo", "a.rs", 1, "function"),
            doc_comment: None,
            language: "rust".into(),
            signature: Some("pub fn foo(x: i32) -> Result<()>".into()),
            body: vec!["    let _ = x;".into(), "    Ok(())".into()],
            body_truncated: false,
            callers: vec![],
        };
        let md = render_markdown("foo", &r);
        assert!(md.contains("**Signature**"));
        assert!(md.contains("pub fn foo(x: i32) -> Result<()>"));
        assert!(md.contains("**Body** (2 lines)"));
        assert!(md.contains("```rust"));
        assert!(md.contains("let _ = x;"));
    }

    #[test]
    fn test_render_markdown_truncated_body_calls_out_truncation() {
        let r = FollowResult {
            focus: mk_node(1, "foo", "a.rs", 1, "function"),
            doc_comment: None,
            language: "rust".into(),
            signature: Some("pub fn foo()".into()),
            body: vec!["one".into(), "two".into()],
            body_truncated: true,
            callers: vec![],
        };
        let md = render_markdown("foo", &r);
        assert!(md.contains("truncated to fit budget"));
        assert!(md.contains("**Body** (2 lines, truncated"));
    }

    #[test]
    fn test_render_markdown_omitted_body_when_truncated_to_zero() {
        let r = FollowResult {
            focus: mk_node(1, "foo", "a.rs", 1, "function"),
            doc_comment: None,
            language: "rust".into(),
            signature: Some("pub fn foo()".into()),
            body: vec![],
            body_truncated: true,
            callers: vec![],
        };
        let md = render_markdown("foo", &r);
        assert!(md.contains("Body omitted"));
    }

    #[test]
    fn test_render_markdown_callers_section_lists_each_caller() {
        let r = FollowResult {
            focus: mk_node(1, "target", "t.rs", 1, "function"),
            doc_comment: None,
            language: "rust".into(),
            signature: Some("pub fn target()".into()),
            body: vec![],
            body_truncated: false,
            callers: vec![
                mk_node(10, "caller_a", "a.rs", 5, "function"),
                mk_node(11, "caller_b", "b.rs", 6, "function"),
            ],
        };
        let md = render_markdown("target", &r);
        assert!(md.contains("**Callers** (2 shown)"));
        assert!(md.contains("`caller_a`"));
        assert!(md.contains("`caller_b`"));
        assert!(md.contains("a.rs:5"));
    }

    #[test]
    fn test_render_markdown_picks_correct_code_fence_per_extension() {
        for (file, want) in [
            ("a.rs", "rust"),
            ("a.py", "python"),
            ("a.ts", "typescript"),
            ("a.js", "javascript"),
            ("a.go", "go"),
        ] {
            let r = FollowResult {
                focus: mk_node(1, "x", file, 1, "function"),
                doc_comment: None,
                language: "rust".into(),
                signature: Some("sig".into()),
                body: vec!["body".into()],
                body_truncated: false,
                callers: vec![],
            };
            let md = render_markdown("x", &r);
            assert!(
                md.contains(&format!("```{want}")),
                "expected fence ```{want} for file {file}; got md: {md}"
            );
        }
    }

    #[test]
    fn test_parse_from_handles_simple_path_and_line() {
        assert_eq!(parse_from("a.rs:42"), Some(("a.rs", 42)));
        assert_eq!(parse_from("src/foo/bar.rs:1"), Some(("src/foo/bar.rs", 1)));
    }

    #[test]
    fn test_parse_from_returns_none_for_malformed_input() {
        assert!(parse_from("a.rs").is_none());
        assert!(parse_from("a.rs:notanumber").is_none());
        assert!(parse_from(":42").is_none_or(|(f, _)| f.is_empty()));
    }
}
