//! `lens refs <symbol>` — list callers (and other reference sites) of a symbol.
//!
//! Resolves the symbol with the same disambiguation rules as `follow`, then
//! delegates to [`lens_core::list_refs`] for the data and renders a markdown
//! card. Output is deterministic.

use std::path::Path;

use lens_core::{list_refs, resolve_symbol_to_id, Graph, RefsResult};

pub fn run(symbol: &str, limit: u32) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens refs: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, symbol, limit)
}

pub fn run_with_root(root: &Path, symbol: &str, limit: u32) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "refs") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let graph = match Graph::load(&storage) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lens refs: failed to load graph: {e}");
            return Err(1);
        }
    };

    let ids = resolve_symbol_to_id(&graph, symbol);
    if ids.is_empty() {
        eprintln!("lens refs: no symbol matched '{symbol}'.");
        return Err(1);
    }
    if ids.len() > 1 {
        eprintln!(
            "lens refs: '{symbol}' is ambiguous ({} candidates). Disambiguate with a qualified name:",
            ids.len()
        );
        for sid in ids.iter().take(10) {
            if let Some(meta) = graph.symbols.get(sid) {
                eprintln!(
                    "  - {} ({} at {}:{})",
                    meta.qualified_name, meta.kind, meta.file_path, meta.start_line
                );
            }
        }
        return Err(1);
    }

    let sid = ids[0];
    let result = match list_refs(&storage, sid, limit) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "lens refs: symbol '{symbol}' resolved to id {sid} but storage lookup returned None"
            );
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens refs: lookup failed: {e}");
            return Err(1);
        }
    };

    print!("{}", render_markdown(symbol, limit, &result));
    Ok(())
}

/// Pure markdown formatter — no fs / no Storage / no network. Safe to unit-test
/// in isolation per the markdown-formatter-purity convention.
pub fn render_markdown(input: &str, limit: u32, result: &RefsResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let f = &result.focus;

    let _ = writeln!(&mut out, "# Refs: `{}`", f.qualified_name);
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Kind:** {} • **Defined at:** `{}:{}` • **Resolved from:** `{}`",
        f.kind, f.file_path, f.start_line, input
    );
    let _ = writeln!(&mut out);

    if result.sites.is_empty() {
        let _ = writeln!(&mut out, "_No references recorded in the index._");
        let _ = writeln!(&mut out);
        return out;
    }

    let header = if result.truncated {
        format!(
            "**{} reference site{} (showing first {}, more exist past --limit {})**",
            result.sites.len(),
            if result.sites.len() == 1 { "" } else { "s" },
            result.sites.len(),
            limit
        )
    } else {
        format!(
            "**{} reference site{}**",
            result.sites.len(),
            if result.sites.len() == 1 { "" } else { "s" }
        )
    };
    let _ = writeln!(&mut out, "{header}");
    let _ = writeln!(&mut out);

    for site in &result.sites {
        let caller_part = match &site.caller {
            Some(c) => format!("`{}` ({})", c.qualified_name, c.kind),
            None => "_<module-level>_".to_string(),
        };
        let _ = writeln!(
            &mut out,
            "- [{kind}] {caller_part} → `{file}:{line}`",
            kind = site.kind,
            caller_part = caller_part,
            file = site.file_path,
            line = site.line
        );
    }
    let _ = writeln!(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::{QueryNode, RefSite};
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
    fn test_refs_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "anything", 100);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_refs_run_errors_on_unknown_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn known() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "ghost", 100);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_refs_run_succeeds_on_orphan_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn lonely() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "lonely", 100);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_refs_run_succeeds_when_callers_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("a.rs"),
            "pub fn target() {}\n\
             pub fn caller() { target(); }\n",
        );
        build_initial_index(root);
        let r = run_with_root(root, "target", 100);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_render_markdown_zero_sites_says_no_references() {
        let r = RefsResult {
            focus: mk_node(1, "x", "a.rs", 1, "function"),
            sites: vec![],
            truncated: false,
        };
        let md = render_markdown("x", 100, &r);
        assert!(md.contains("# Refs: `x`"));
        assert!(md.contains("No references recorded"));
    }

    #[test]
    fn test_render_markdown_lists_each_site_with_caller_kind_file_line() {
        let r = RefsResult {
            focus: mk_node(1, "target", "t.rs", 1, "function"),
            sites: vec![
                RefSite {
                    caller: Some(mk_node(10, "caller_a", "a.rs", 5, "function")),
                    kind: "call".into(),
                    file_path: "a.rs".into(),
                    line: 8,
                    col: 4,
                },
                RefSite {
                    caller: Some(mk_node(11, "caller_b", "b.rs", 3, "function")),
                    kind: "call".into(),
                    file_path: "b.rs".into(),
                    line: 6,
                    col: 0,
                },
            ],
            truncated: false,
        };
        let md = render_markdown("target", 100, &r);
        assert!(md.contains("# Refs: `target`"));
        assert!(md.contains("**2 reference sites**"));
        assert!(md.contains("[call] `caller_a`"));
        assert!(md.contains("a.rs:8"));
        assert!(md.contains("[call] `caller_b`"));
        assert!(md.contains("b.rs:6"));
    }

    #[test]
    fn test_render_markdown_truncated_calls_out_more_exist() {
        let r = RefsResult {
            focus: mk_node(1, "x", "a.rs", 1, "function"),
            sites: vec![RefSite {
                caller: Some(mk_node(10, "c1", "a.rs", 5, "function")),
                kind: "call".into(),
                file_path: "a.rs".into(),
                line: 8,
                col: 4,
            }],
            truncated: true,
        };
        let md = render_markdown("x", 1, &r);
        assert!(md.contains("more exist past --limit 1"));
    }

    #[test]
    fn test_render_markdown_handles_module_level_ref_with_none_caller() {
        let r = RefsResult {
            focus: mk_node(1, "x", "a.rs", 1, "function"),
            sites: vec![RefSite {
                caller: None,
                kind: "type".into(),
                file_path: "m.rs".into(),
                line: 2,
                col: 0,
            }],
            truncated: false,
        };
        let md = render_markdown("x", 100, &r);
        assert!(md.contains("[type] _<module-level>_"));
        assert!(md.contains("m.rs:2"));
    }
}
