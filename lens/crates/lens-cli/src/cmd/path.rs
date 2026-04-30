//! `lens path` — shortest path between two symbols. graphify-parity for
//! `graphify path "A" "B"`.

use std::path::Path;

use lens_core::{
    resolve_symbol_to_id, shortest_path, EdgeKind, Graph, PathResult, QueryEdge, QueryNode,
};

pub fn run(from: &str, to: &str) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens path: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, from, to)
}

pub fn run_with_root(root: &Path, from: &str, to: &str) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "path") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let graph = match Graph::load(&storage) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lens path: failed to load graph: {e}");
            return Err(1);
        }
    };

    let from_ids = resolve_symbol_to_id(&graph, from);
    let to_ids = resolve_symbol_to_id(&graph, to);

    if from_ids.is_empty() {
        eprintln!("lens path: no symbol matched '{from}'.");
        return Err(1);
    }
    if to_ids.is_empty() {
        eprintln!("lens path: no symbol matched '{to}'.");
        return Err(1);
    }
    if from_ids.len() > 1 {
        eprintln!(
            "lens path: '{}' is ambiguous ({} candidates). Disambiguate with a qualified name:",
            from, from_ids.len()
        );
        for sid in from_ids.iter().take(10) {
            if let Some(meta) = graph.symbols.get(sid) {
                eprintln!("  - {} ({} at {}:{})",
                    meta.qualified_name, meta.kind, meta.file_path, meta.start_line);
            }
        }
        return Err(1);
    }
    if to_ids.len() > 1 {
        eprintln!(
            "lens path: '{}' is ambiguous ({} candidates). Disambiguate with a qualified name:",
            to, to_ids.len()
        );
        for sid in to_ids.iter().take(10) {
            if let Some(meta) = graph.symbols.get(sid) {
                eprintln!("  - {} ({} at {}:{})",
                    meta.qualified_name, meta.kind, meta.file_path, meta.start_line);
            }
        }
        return Err(1);
    }

    let result = match shortest_path(&graph, from_ids[0], to_ids[0]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lens path: traversal failed: {e}");
            return Err(1);
        }
    };

    let rendered = render_markdown(from, to, result.as_ref());
    print!("{rendered}");
    Ok(())
}

pub fn render_markdown(from: &str, to: &str, result: Option<&PathResult>) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Path: {from} → {to}");
    let _ = writeln!(&mut out);

    let Some(p) = result else {
        let _ = writeln!(&mut out, "_No path found between the two symbols._");
        return out;
    };

    if p.distance == 0 {
        let _ = writeln!(&mut out, "_Source and destination are the same symbol._");
        if let Some(n) = p.nodes.first() {
            let _ = writeln!(&mut out, "- `{}` ({}) — `{}:{}`",
                n.qualified_name, n.kind, n.file_path, n.start_line);
        }
        return out;
    }

    let _ = writeln!(&mut out, "**Distance:** {} edge{}",
        p.distance, if p.distance == 1 { "" } else { "s" });
    let _ = writeln!(&mut out);
    let _ = writeln!(&mut out, "## Hops");
    for (i, n) in p.nodes.iter().enumerate() {
        let _ = writeln!(
            &mut out,
            "{}. `{}` ({}) — `{}:{}`",
            i + 1, n.qualified_name, n.kind, n.file_path, n.start_line
        );
        if i < p.edges.len() {
            let e = &p.edges[i];
            let arrow = arrow_for(e, n);
            let _ = writeln!(&mut out, "   {arrow} via {}", edge_label(e.kind));
        }
    }
    let _ = writeln!(&mut out);
    out
}

fn arrow_for(edge: &QueryEdge, current: &QueryNode) -> &'static str {
    if edge.from_symbol_id == current.symbol_id {
        "→"
    } else {
        "←"
    }
}

fn edge_label(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Parent => "parent",
        EdgeKind::Call => "call",
        EdgeKind::Type => "type",
        EdgeKind::Import => "import",
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

    #[test]
    fn test_path_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "a", "b");
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_path_run_errors_when_from_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn known() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "ghost", "known");
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_path_run_succeeds_for_self_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn only() {}\n");
        build_initial_index(root);
        let r = run_with_root(root, "only", "only");
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_render_markdown_no_result_includes_helpful_message() {
        let md = render_markdown("a", "b", None);
        assert!(md.contains("# Path: a → b"));
        assert!(md.contains("No path found"));
    }

    #[test]
    fn test_render_markdown_self_path_includes_distance_zero_note() {
        let p = PathResult {
            nodes: vec![QueryNode {
                symbol_id: 1, qualified_name: "x".into(), name: "x".into(),
                kind: "function".into(), file_path: "a.rs".into(), start_line: 1,
                is_seed: false,
            }],
            edges: vec![],
            distance: 0,
        };
        let md = render_markdown("x", "x", Some(&p));
        assert!(md.contains("same symbol"));
        assert!(md.contains("`x`"));
    }

    #[test]
    fn test_render_markdown_multi_hop_lists_ordered_hops() {
        let p = PathResult {
            nodes: vec![
                QueryNode {
                    symbol_id: 1, qualified_name: "a".into(), name: "a".into(),
                    kind: "function".into(), file_path: "a.rs".into(), start_line: 1,
                    is_seed: false,
                },
                QueryNode {
                    symbol_id: 2, qualified_name: "b".into(), name: "b".into(),
                    kind: "function".into(), file_path: "b.rs".into(), start_line: 1,
                    is_seed: false,
                },
                QueryNode {
                    symbol_id: 3, qualified_name: "c".into(), name: "c".into(),
                    kind: "function".into(), file_path: "c.rs".into(), start_line: 1,
                    is_seed: false,
                },
            ],
            edges: vec![
                QueryEdge { from_symbol_id: 1, to_symbol_id: 2, kind: EdgeKind::Call },
                QueryEdge { from_symbol_id: 2, to_symbol_id: 3, kind: EdgeKind::Import },
            ],
            distance: 2,
        };
        let md = render_markdown("a", "c", Some(&p));
        assert!(md.contains("Distance:** 2 edges"));
        assert!(md.contains("1. `a`"));
        assert!(md.contains("2. `b`"));
        assert!(md.contains("3. `c`"));
        assert!(md.contains("via call"));
        assert!(md.contains("via import"));
    }
}
