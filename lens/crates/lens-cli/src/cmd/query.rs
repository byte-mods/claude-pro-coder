//! `lens query` — graphify-parity question-driven graph traversal.
//!
//! Wires the `lens-core::query` module to the CLI and emits a markdown report.
//!
//! Behavioural contract:
//!   - exit 1 if `.lens/index.db` is missing (run `lens index` first);
//!   - exit 0 with empty markdown body if the question matches no symbols;
//!   - markdown output: a `Query` heading naming the question, a `Seeds`
//!     section listing seed symbols (the question's direct matches), an
//!     `Expanded` section listing other reached symbols grouped by file,
//!     and a footer line summarising mode + budget + token estimate.

use std::path::Path;

use lens_core::{
    query_graph, EdgeKind, QueryNode, QueryResult, TraversalMode,
};

/// Run `lens query` against the index in the current working directory's
/// `.lens/`. The question is `question`; mode is BFS unless `dfs` is true;
/// the token budget caps how much is rendered.
pub fn run(question: &str, dfs: bool, budget: u32) -> Result<(), u8> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens query: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    run_with_root(&cwd, question, dfs, budget)
}

/// Test-friendly variant — caller specifies the project root explicitly.
pub fn run_with_root(root: &Path, question: &str, dfs: bool, budget: u32) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "query") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };

    let mode = if dfs { TraversalMode::Dfs } else { TraversalMode::Bfs };
    let result = match query_graph(&storage, question, mode, budget) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lens query: traversal failed: {e}");
            return Err(1);
        }
    };

    let rendered = render_markdown(question, &result);
    print!("{rendered}");
    Ok(())
}

/// Render a [`QueryResult`] as a graphify-style markdown answer. Pure;
/// callable from tests without spinning up a full process.
pub fn render_markdown(question: &str, result: &QueryResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Query: {question}");
    let _ = writeln!(&mut out);

    if result.seeds.is_empty() {
        let _ = writeln!(
            &mut out,
            "_No symbols matched the question. Try a more specific name or qualifier._"
        );
        return out;
    }

    let _ = writeln!(&mut out, "## Seeds ({})", result.seeds.len());
    for n in result.nodes.iter().filter(|n| n.is_seed) {
        let _ = writeln!(&mut out, "- `{}` ({}) — `{}:{}`",
            n.qualified_name, n.kind, n.file_path, n.start_line);
    }
    let _ = writeln!(&mut out);

    let expanded: Vec<&QueryNode> = result.nodes.iter().filter(|n| !n.is_seed).collect();
    if !expanded.is_empty() {
        let _ = writeln!(&mut out, "## Expanded ({})", expanded.len());
        // Group by file for readability — same file gets a single bullet per
        // entry, sorted by line.
        let mut by_file: std::collections::BTreeMap<&str, Vec<&QueryNode>> = std::collections::BTreeMap::new();
        for n in &expanded {
            by_file.entry(n.file_path.as_str()).or_default().push(n);
        }
        for (file, mut nodes) in by_file {
            nodes.sort_by_key(|n| n.start_line);
            let _ = writeln!(&mut out, "- `{file}`");
            for n in nodes {
                let _ = writeln!(
                    &mut out,
                    "  - `{}` ({}) — line {}",
                    n.qualified_name, n.kind, n.start_line
                );
            }
        }
        let _ = writeln!(&mut out);
    }

    if !result.edges.is_empty() {
        let edge_summary = summarise_edges(result);
        let _ = writeln!(&mut out, "## Edges traversed");
        let _ = writeln!(
            &mut out,
            "- {} parent / {} call / {} type / {} import",
            edge_summary.parent, edge_summary.call, edge_summary.type_rel, edge_summary.import
        );
        let _ = writeln!(&mut out);
    }

    let mode = match result.mode {
        Some(TraversalMode::Bfs) => "bfs",
        Some(TraversalMode::Dfs) => "dfs",
        None => "n/a",
    };
    let _ = writeln!(
        &mut out,
        "_traversal: {mode}, budget: {} tokens, est: {} tokens, nodes: {}, edges: {}_",
        result.budget,
        result.estimated_tokens,
        result.nodes.len(),
        result.edges.len()
    );
    out
}

#[derive(Default)]
struct EdgeBreakdown {
    parent: u32,
    call: u32,
    type_rel: u32,
    import: u32,
}

fn summarise_edges(r: &QueryResult) -> EdgeBreakdown {
    let mut b = EdgeBreakdown::default();
    for e in &r.edges {
        match e.kind {
            EdgeKind::Parent => b.parent += 1,
            EdgeKind::Call => b.call += 1,
            EdgeKind::Type => b.type_rel += 1,
            EdgeKind::Import => b.import += 1,
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::QueryNode;
    use std::fs;

    fn build_initial_index(root: &Path) {
        crate::cmd::index::run(Some(root)).expect("initial index");
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn test_query_run_errors_when_lens_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), "anything", false, 1000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_query_run_against_indexed_project_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn callee() {}\n");
        write(
            &root.join("b.rs"),
            "fn caller() { let _ = a::callee(); }\n",
        );
        build_initial_index(root);
        let r = run_with_root(root, "callee", false, 5000);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_render_markdown_no_seeds_includes_helpful_message() {
        let r = QueryResult::default();
        let md = render_markdown("flux capacitor", &r);
        assert!(md.contains("# Query: flux capacitor"));
        assert!(md.contains("No symbols matched"));
    }

    #[test]
    fn test_render_markdown_seeds_section_lists_seed_qnames() {
        let mut r = QueryResult {
            seeds: vec![1],
            mode: Some(TraversalMode::Bfs),
            budget: 1000,
            ..Default::default()
        };
        r.nodes.push(QueryNode {
            symbol_id: 1,
            qualified_name: "crate::foo".into(),
            name: "foo".into(),
            kind: "function".into(),
            file_path: "src/lib.rs".into(),
            start_line: 7,
            is_seed: true,
        });
        let md = render_markdown("foo", &r);
        assert!(md.contains("## Seeds (1)"));
        assert!(md.contains("`crate::foo` (function) — `src/lib.rs:7`"));
    }

    #[test]
    fn test_render_markdown_expanded_groups_by_file() {
        let r = QueryResult {
            seeds: vec![1],
            mode: Some(TraversalMode::Bfs),
            budget: 1000,
            nodes: vec![
                QueryNode {
                    symbol_id: 1, qualified_name: "x::seed".into(), name: "seed".into(),
                    kind: "function".into(), file_path: "x.rs".into(), start_line: 1,
                    is_seed: true,
                },
                QueryNode {
                    symbol_id: 2, qualified_name: "y::a".into(), name: "a".into(),
                    kind: "function".into(), file_path: "y.rs".into(), start_line: 5,
                    is_seed: false,
                },
                QueryNode {
                    symbol_id: 3, qualified_name: "y::b".into(), name: "b".into(),
                    kind: "function".into(), file_path: "y.rs".into(), start_line: 10,
                    is_seed: false,
                },
            ],
            ..Default::default()
        };
        let md = render_markdown("seed", &r);
        assert!(md.contains("## Expanded (2)"));
        // y.rs gets one parent bullet with both children indented under it.
        let y_idx = md.find("- `y.rs`").expect("expected y.rs heading");
        let a_idx = md.find("`y::a`").expect("expected y::a entry");
        let b_idx = md.find("`y::b`").expect("expected y::b entry");
        assert!(y_idx < a_idx && a_idx < b_idx, "expected file-grouped order");
    }

    #[test]
    fn test_render_markdown_edges_summary_present_when_any_edge() {
        let r = QueryResult {
            seeds: vec![1],
            mode: Some(TraversalMode::Bfs),
            budget: 100,
            edges: vec![
                lens_core::QueryEdge { from_symbol_id: 1, to_symbol_id: 2, kind: EdgeKind::Call },
                lens_core::QueryEdge { from_symbol_id: 2, to_symbol_id: 3, kind: EdgeKind::Parent },
            ],
            ..Default::default()
        };
        let md = render_markdown("x", &r);
        assert!(md.contains("## Edges traversed"));
        assert!(md.contains("1 parent / 1 call / 0 type / 0 import"));
    }

    #[test]
    fn test_render_markdown_footer_includes_mode_and_budget() {
        let r = QueryResult {
            seeds: vec![1],
            mode: Some(TraversalMode::Dfs),
            budget: 250,
            estimated_tokens: 80,
            nodes: vec![QueryNode {
                symbol_id: 1, qualified_name: "x".into(), name: "x".into(),
                kind: "function".into(), file_path: "a.rs".into(), start_line: 1,
                is_seed: true,
            }],
            ..Default::default()
        };
        let md = render_markdown("x", &r);
        assert!(md.contains("traversal: dfs"));
        assert!(md.contains("budget: 250"));
        assert!(md.contains("est: 80"));
    }
}
