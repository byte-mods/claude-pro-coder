//! `lens explain` — node card with neighbors. graphify-parity for
//! `graphify explain "X"`.

use std::path::Path;

use lens_core::{
    explain_symbol, resolve_symbol_to_id, ExplainResult, Graph, QueryNode,
};

pub fn run(symbol: &str) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens explain: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, symbol)
}

pub fn run_with_root(root: &Path, symbol: &str) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "explain") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let graph = match Graph::load(&storage) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lens explain: failed to load graph: {e}");
            return Err(1);
        }
    };

    let ids = resolve_symbol_to_id(&graph, symbol);
    if ids.is_empty() {
        eprintln!("lens explain: no symbol matched '{symbol}'.");
        return Err(1);
    }
    if ids.len() > 1 {
        eprintln!(
            "lens explain: '{}' is ambiguous ({} candidates). Disambiguate with a qualified name:",
            symbol, ids.len()
        );
        for sid in ids.iter().take(10) {
            if let Some(meta) = graph.symbols.get(sid) {
                eprintln!("  - {} ({} at {}:{})",
                    meta.qualified_name, meta.kind, meta.file_path, meta.start_line);
            }
        }
        return Err(1);
    }

    let result = match explain_symbol(&graph, ids[0]) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("lens explain: symbol '{symbol}' resolved to id {} but graph load lost it",
                ids[0]);
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens explain: lookup failed: {e}");
            return Err(1);
        }
    };

    print!("{}", render_markdown(&result));
    Ok(())
}

pub fn render_markdown(result: &ExplainResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let f = &result.focus;
    let _ = writeln!(&mut out, "# Explain: `{}`", f.qualified_name);
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Kind:** {} • **File:** `{}:{}`",
        f.kind, f.file_path, f.start_line
    );
    let _ = writeln!(&mut out);

    if result.is_isolated() {
        let _ = writeln!(
            &mut out,
            "_This symbol has no parents, children, callers, callees, type relations, or imports recorded in the index._"
        );
        return out;
    }

    let _ = writeln!(
        &mut out,
        "**Neighbors:** {} total ({} parent / {} child / {} caller / {} callee / {} type / {} import)",
        result.total_neighbors(),
        result.parents.len(),
        result.children.len(),
        result.callers.len(),
        result.callees.len(),
        result.types.len(),
        result.imports.len()
    );
    let _ = writeln!(&mut out);

    write_section(&mut out, "Parents", &result.parents);
    write_section(&mut out, "Children", &result.children);
    write_section(&mut out, "Callers", &result.callers);
    write_section(&mut out, "Callees", &result.callees);
    write_section(&mut out, "Types", &result.types);
    write_section(&mut out, "Imports", &result.imports);
    out
}

fn write_section(out: &mut String, title: &str, nodes: &[QueryNode]) {
    use std::fmt::Write;
    if nodes.is_empty() {
        return;
    }
    let _ = writeln!(out, "## {title} ({})", nodes.len());
    for n in nodes {
        let _ = writeln!(
            out,
            "- `{}` ({}) — `{}:{}`",
            n.qualified_name, n.kind, n.file_path, n.start_line
        );
    }
    let _ = writeln!(out);
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

    #[test]
    fn test_explain_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "x");
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_explain_run_errors_on_unknown_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn known() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "ghost");
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_explain_run_succeeds_for_known_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn alone() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "alone");
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_render_markdown_isolated_includes_helpful_message() {
        let r = ExplainResult {
            focus: QueryNode {
                symbol_id: 1, qualified_name: "x".into(), name: "x".into(),
                kind: "function".into(), file_path: "a.rs".into(), start_line: 1,
                is_seed: true,
            },
            parents: vec![], children: vec![], callers: vec![], callees: vec![],
            types: vec![], imports: vec![],
        };
        let md = render_markdown(&r);
        assert!(md.contains("# Explain: `x`"));
        assert!(md.contains("no parents"));
    }

    #[test]
    fn test_render_markdown_with_neighbors_sections_present() {
        let mk = |id: i64, q: &str| QueryNode {
            symbol_id: id, qualified_name: q.into(), name: q.into(),
            kind: "function".into(), file_path: "x.rs".into(), start_line: 1,
            is_seed: false,
        };
        let r = ExplainResult {
            focus: QueryNode {
                symbol_id: 1, qualified_name: "Owner".into(), name: "Owner".into(),
                kind: "struct".into(), file_path: "a.rs".into(), start_line: 1,
                is_seed: true,
            },
            parents: vec![],
            children: vec![mk(2, "Owner::method")],
            callers: vec![],
            callees: vec![mk(3, "callee")],
            types: vec![],
            imports: vec![],
        };
        let md = render_markdown(&r);
        assert!(md.contains("Neighbors:** 2 total"));
        assert!(md.contains("## Children (1)"));
        assert!(md.contains("## Callees (1)"));
        assert!(!md.contains("## Parents")); // empty section omitted
    }
}
