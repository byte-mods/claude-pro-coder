//! `lens slice <file:line>` — minimal context for a file:line location.
//! Parses the user-facing `path:line` string, resolves the smallest enclosing
//! symbol via `lens_core::slice_at`, and renders a clean markdown card.

use std::path::Path;

use lens_core::{slice_at, SliceResult};

pub fn run(location: &str, budget: u32) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens slice: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, location, budget)
}

pub fn run_with_root(root: &Path, location: &str, budget: u32) -> Result<(), u8> {
    let Some((file_path, line)) = parse_location(location) else {
        eprintln!("lens slice: location must be of the form FILE:LINE (got '{location}').");
        return Err(2);
    };

    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "slice") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };

    let result = match slice_at(&storage, root, file_path, line, budget) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "lens slice: no symbol encloses '{file_path}:{line}'. \
                 Either the file is not indexed or the line is outside any symbol's range."
            );
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens slice: lookup failed: {e}");
            return Err(1);
        }
    };

    print!("{}", render_markdown(location, &result));
    Ok(())
}

/// Pure markdown formatter — no fs / no Storage / no network. Per the
/// markdown-formatter-purity convention, callable in tests without a tempdir.
pub fn render_markdown(input: &str, result: &SliceResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let f = &result.focus;

    let _ = writeln!(&mut out, "# Slice: `{input}`");
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Encloses:** `{}` ({}) • **File:** `{}:{}`",
        f.qualified_name, f.kind, f.file_path, f.start_line
    );
    let _ = writeln!(&mut out);

    if !result.imports.is_empty() {
        let _ = writeln!(
            &mut out,
            "**Imports** ({} shown)",
            result.imports.len()
        );
        let _ = writeln!(&mut out);
        let _ = writeln!(&mut out, "```{}", code_fence_lang(&f.file_path));
        let prefix = import_prefix(&f.file_path);
        for raw in &result.imports {
            let _ = writeln!(&mut out, "{prefix}{raw};");
        }
        let _ = writeln!(&mut out, "```");
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
            let _ = writeln!(
                &mut out,
                "**Body** ({} lines, truncated to fit budget)",
                result.body.len()
            );
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

    out
}

fn parse_location(s: &str) -> Option<(&str, u32)> {
    // Rightmost-`:` split — allows paths containing `:` (e.g. Windows
    // drive letters) so long as the line number is the final segment.
    let (file, line) = s.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    let line: u32 = line.parse().ok()?;
    Some((file, line))
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

fn import_prefix(file_path: &str) -> &'static str {
    match Path::new(file_path).extension().and_then(|s| s.to_str()) {
        Some("rs") => "use ",
        Some("py") => "import ",
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

    fn mk_node(qname: &str, file: &str, line: i64, kind: &str) -> QueryNode {
        QueryNode {
            symbol_id: 1,
            qualified_name: qname.into(),
            name: qname.rsplit("::").next().unwrap_or(qname).into(),
            kind: kind.into(),
            file_path: file.into(),
            start_line: line,
            is_seed: true,
        }
    }

    #[test]
    fn test_slice_run_errors_on_malformed_location() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "no_colon_here", 1000);
        assert_eq!(r, Err(2));
    }

    #[test]
    fn test_slice_run_errors_on_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), ":42", 1000);
        assert_eq!(r, Err(2));
    }

    #[test]
    fn test_slice_run_errors_on_nonnumeric_line() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "a.rs:abc", 1000);
        assert_eq!(r, Err(2));
    }

    #[test]
    fn test_slice_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "a.rs:1", 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_slice_run_errors_when_no_enclosing_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn known() {}\n");
        build_initial_index(root);
        // Line 99 is past the file; no symbol encloses it.
        let r = run_with_root(root, "a.rs:99", 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_slice_run_succeeds_for_line_inside_known_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("a.rs"),
            "pub fn body_holder() {\n    let x = 1;\n}\n",
        );
        build_initial_index(root);
        // Line 1 is the start of the fn → enclosed.
        let r = run_with_root(root, "a.rs:1", 5000);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_render_markdown_minimal_no_imports_no_signature_no_body() {
        let r = SliceResult {
            focus: mk_node("x", "a.rs", 1, "function"),
            signature: None,
            body: vec![],
            body_truncated: false,
            imports: vec![],
        };
        let md = render_markdown("a.rs:1", &r);
        assert!(md.contains("# Slice: `a.rs:1`"));
        assert!(md.contains("**Encloses:** `x` (function)"));
        assert!(!md.contains("**Imports**"));
        assert!(!md.contains("**Signature**"));
        assert!(!md.contains("**Body**"));
    }

    #[test]
    fn test_render_markdown_full_card_includes_all_sections_in_order() {
        let r = SliceResult {
            focus: mk_node("foo::bar", "a.rs", 5, "function"),
            signature: Some("pub fn bar()".into()),
            body: vec!["    let _ = 1;".into()],
            body_truncated: false,
            imports: vec!["std::fs".into(), "std::path::Path".into()],
        };
        let md = render_markdown("a.rs:6", &r);
        let pos_imports = md.find("**Imports**").unwrap();
        let pos_signature = md.find("**Signature**").unwrap();
        let pos_body = md.find("**Body**").unwrap();
        assert!(pos_imports < pos_signature, "imports must come before signature");
        assert!(pos_signature < pos_body, "signature must come before body");
        assert!(md.contains("use std::fs;"));
        assert!(md.contains("use std::path::Path;"));
        assert!(md.contains("pub fn bar()"));
        assert!(md.contains("let _ = 1;"));
    }

    #[test]
    fn test_render_markdown_python_imports_use_python_keyword() {
        let r = SliceResult {
            focus: mk_node("pkg.module.foo", "pkg/module.py", 1, "function"),
            signature: Some("def foo():".into()),
            body: vec![],
            body_truncated: false,
            imports: vec!["os".into(), "sys".into()],
        };
        let md = render_markdown("pkg/module.py:1", &r);
        assert!(md.contains("import os;"));
        assert!(md.contains("import sys;"));
        assert!(md.contains("```python"));
    }

    #[test]
    fn test_render_markdown_truncated_body_calls_out_truncation() {
        let r = SliceResult {
            focus: mk_node("x", "a.rs", 1, "function"),
            signature: Some("pub fn x()".into()),
            body: vec!["one".into(), "two".into()],
            body_truncated: true,
            imports: vec![],
        };
        let md = render_markdown("a.rs:2", &r);
        assert!(md.contains("**Body** (2 lines, truncated to fit budget)"));
    }

    #[test]
    fn test_render_markdown_omits_body_when_truncated_to_zero() {
        let r = SliceResult {
            focus: mk_node("x", "a.rs", 1, "function"),
            signature: Some("pub fn x()".into()),
            body: vec![],
            body_truncated: true,
            imports: vec![],
        };
        let md = render_markdown("a.rs:2", &r);
        assert!(md.contains("Body omitted"));
    }

    #[test]
    fn test_parse_location_handles_simple_input() {
        assert_eq!(parse_location("a.rs:42"), Some(("a.rs", 42)));
        assert_eq!(parse_location("src/foo/bar.rs:1"), Some(("src/foo/bar.rs", 1)));
    }

    #[test]
    fn test_parse_location_returns_none_for_malformed() {
        assert!(parse_location("a.rs").is_none());
        assert!(parse_location("a.rs:abc").is_none());
        assert!(parse_location(":42").is_none());
        assert!(parse_location("").is_none());
    }
}
