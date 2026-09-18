//! Shortest-path queries over the symbol graph — graphify-parity for
//! `graphify path "A" "B"`.
//!
//! Builds on [`crate::query::Graph`]: load the graph once, then BFS from
//! the source symbol_id until the destination is found. Returns the path
//! as an ordered `Vec<QueryNode>` plus the edges that connect them.
//!
//! ## Why BFS, not Dijkstra
//!
//! All edges in the symbol graph have equal weight (no quantitative
//! "distance" — every connection means *somehow related*). BFS is optimal
//! for shortest paths in unweighted graphs. If a future edge gets a weight
//! (e.g. "import depth" or "language boundary cost"), upgrade to Dijkstra.
//!
//! ## Disambiguation
//!
//! Path arguments are free-form strings — `"create_order"`, `"foo::bar"`,
//! `"OrderService::handle"`. [`resolve_symbol_to_id`] picks the best match:
//!
//! 1. exact qualified-name match,
//! 2. exact name match (deterministic by lowest symbol_id on ties),
//! 3. substring of qualified_name (most specific, lowest symbol_id on ties).
//!
//! Returns `Vec<i64>` so callers can detect ambiguity. The CLI surfaces
//! ambiguity by listing candidates and asking the user to qualify.

use std::collections::{HashMap, VecDeque};

use crate::error::{LensError, Result};
use crate::query::{EdgeKind, Graph, QueryEdge, QueryNode};
use crate::storage::Storage;

/// One candidate produced by [`resolve_symbol_candidates`] — the same
/// shape as [`crate::query::SymbolMeta`] plus the id, so CLI verbs can
/// print an ambiguity listing without loading the whole graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolCandidate {
    pub symbol_id: i64,
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: i64,
    pub language: String,
}

/// SQL-backed twin of [`resolve_symbol_to_id`] with identical tier
/// semantics (exact qname → exact name → `::name`/`.name` suffix →
/// substring), returning full candidate rows. `lens follow` / `lens refs`
/// use this instead of `Graph::load` so resolving one symbol costs a few
/// indexed lookups rather than a scan of every symbol, call, type and
/// import row — the difference between milliseconds and seconds on a
/// 100K-symbol index.
pub fn resolve_symbol_candidates(storage: &Storage, query: &str) -> Result<Vec<SymbolCandidate>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    const SELECT: &str = "SELECT s.id, s.qualified_name, s.name, s.kind, f.path, s.start_line, f.language
                          FROM symbols s JOIN files f ON f.id = s.file_id";
    // `substr` with a negative start counts from the end; comparing against
    // the separator + query avoids LIKE's `_` wildcard pitfalls.
    let tiers: [String; 4] = [
        format!("{SELECT} WHERE s.qualified_name = ?1 ORDER BY s.id ASC"),
        format!("{SELECT} WHERE s.name = ?1 ORDER BY s.id ASC"),
        format!(
            "{SELECT} WHERE substr(s.qualified_name, -length(?1) - 2) = '::' || ?1
                        OR substr(s.qualified_name, -length(?1) - 1) = '.' || ?1
             ORDER BY s.id ASC"
        ),
        format!("{SELECT} WHERE instr(s.qualified_name, ?1) > 0 ORDER BY s.id ASC"),
    ];
    let conn = storage.connection();
    for sql in &tiers {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| LensError::other(format!("resolve: prepare: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![query], |r| {
                Ok(SymbolCandidate {
                    symbol_id: r.get(0)?,
                    qualified_name: r.get(1)?,
                    name: r.get(2)?,
                    kind: r.get(3)?,
                    file_path: r.get(4)?,
                    start_line: r.get(5)?,
                    language: r.get(6)?,
                })
            })
            .map_err(|e| LensError::other(format!("resolve: query: {e}")))?;
        let found: Vec<SymbolCandidate> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LensError::other(format!("resolve: collect: {e}")))?;
        if !found.is_empty() {
            return Ok(found);
        }
    }
    Ok(Vec::new())
}

/// One shortest path between two symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathResult {
    /// Ordered sequence of symbols on the path, starting with the source
    /// and ending with the destination. Empty only when source == destination.
    pub nodes: Vec<QueryNode>,
    /// Edges connecting consecutive nodes (so `edges.len() == nodes.len() - 1`
    /// when `nodes` has at least two entries).
    pub edges: Vec<QueryEdge>,
    /// Number of edges between source and destination. Zero when source ==
    /// destination, `usize::MAX` is never returned (use `Option<PathResult>`
    /// to signal "no path").
    pub distance: u32,
}

/// BFS shortest path from `from` to `to` over the symbol graph in `graph`.
/// Returns `Ok(None)` when no path exists, `Ok(Some(PathResult))` otherwise.
/// Errors only on internal consistency violations (currently none — the
/// `Result` is reserved for future failure modes like graph-load errors
/// pushed up from a streaming variant).
pub fn shortest_path(graph: &Graph, from: i64, to: i64) -> Result<Option<PathResult>> {
    if !graph.symbols.contains_key(&from) || !graph.symbols.contains_key(&to) {
        return Ok(None);
    }
    if from == to {
        let n = node_for(graph, from);
        return Ok(Some(PathResult {
            nodes: vec![n],
            edges: Vec::new(),
            distance: 0,
        }));
    }

    // BFS with parent-pointer reconstruction. Visited stores Some(prev_id, edge_kind)
    // so we can walk back from destination to source.
    let mut visited: HashMap<i64, Option<(i64, EdgeKind)>> = HashMap::new();
    visited.insert(from, None);
    let mut queue: VecDeque<i64> = VecDeque::new();
    queue.push_back(from);

    while let Some(current) = queue.pop_front() {
        if current == to {
            return Ok(Some(reconstruct(graph, from, to, &visited)));
        }
        if let Some(neighbors) = graph.adjacency.get(&current) {
            for &(nbr, kind) in neighbors {
                if let std::collections::hash_map::Entry::Vacant(slot) = visited.entry(nbr) {
                    slot.insert(Some((current, kind)));
                    queue.push_back(nbr);
                }
            }
        }
    }

    Ok(None)
}

fn reconstruct(
    graph: &Graph,
    from: i64,
    to: i64,
    visited: &HashMap<i64, Option<(i64, EdgeKind)>>,
) -> PathResult {
    let mut node_ids: Vec<i64> = Vec::new();
    let mut edges_rev: Vec<QueryEdge> = Vec::new();
    let mut cur = to;
    loop {
        node_ids.push(cur);
        match visited.get(&cur) {
            Some(Some((prev, kind))) => {
                edges_rev.push(QueryEdge {
                    from_symbol_id: *prev,
                    to_symbol_id: cur,
                    kind: *kind,
                });
                cur = *prev;
                if cur == from {
                    node_ids.push(from);
                    break;
                }
            }
            _ => break,
        }
    }
    node_ids.reverse();
    edges_rev.reverse();

    PathResult {
        distance: edges_rev.len() as u32,
        nodes: node_ids.into_iter().map(|sid| node_for(graph, sid)).collect(),
        edges: edges_rev,
    }
}

fn node_for(graph: &Graph, sid: i64) -> QueryNode {
    // The hard rule: no `expect` on production paths. The BFS reaches only
    // symbol_ids that exist in `graph.adjacency`, which `Graph::load` builds
    // from the same row scan that populates `graph.symbols` — so a missing
    // entry would indicate index corruption, not a precondition violation.
    // Return a placeholder rather than panic; the caller can detect it via
    // empty qualified_name + name if it ever happens.
    if let Some(meta) = graph.symbols.get(&sid) {
        QueryNode {
            symbol_id: sid,
            qualified_name: meta.qualified_name.clone(),
            name: meta.name.clone(),
            kind: meta.kind.clone(),
            file_path: meta.file_path.clone(),
            start_line: meta.start_line,
            is_seed: false,
        }
    } else {
        QueryNode {
            symbol_id: sid,
            qualified_name: String::new(),
            name: String::new(),
            kind: String::new(),
            file_path: String::new(),
            start_line: 0,
            is_seed: false,
        }
    }
}

/// Resolve a free-form symbol string to one or more symbol_ids. Used by
/// `lens path` and `lens explain` to convert CLI args into graph nodes.
///
/// Resolution tiers (best to worst, returned as the first non-empty tier):
///
/// 1. `qualified_name == query` (exact qname).
/// 2. `name == query` (exact name).
/// 3. `qualified_name` ends with `::query` or `.query` (e.g. user typed
///    `foo::bar`, matches `crate::foo::bar`).
/// 4. `qualified_name` contains `query` (substring; last resort).
///
/// Within each tier, results are sorted by `symbol_id` ascending for
/// deterministic ordering. Empty input returns empty result.
pub fn resolve_symbol_to_id(graph: &Graph, query: &str) -> Vec<i64> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut tier1: Vec<i64> = Vec::new(); // exact qname
    let mut tier2: Vec<i64> = Vec::new(); // exact name
    let mut tier3: Vec<i64> = Vec::new(); // qname ends-with ::query or .query
    let mut tier4: Vec<i64> = Vec::new(); // qname substring

    let suffix_colon = format!("::{query}");
    let suffix_dot = format!(".{query}");

    for (sid, meta) in &graph.symbols {
        if meta.qualified_name == query {
            tier1.push(*sid);
            continue;
        }
        if meta.name == query {
            tier2.push(*sid);
            continue;
        }
        if meta.qualified_name.ends_with(&suffix_colon)
            || meta.qualified_name.ends_with(&suffix_dot)
        {
            tier3.push(*sid);
            continue;
        }
        if meta.qualified_name.contains(query) {
            tier4.push(*sid);
        }
    }

    for v in [&mut tier1, &mut tier2, &mut tier3, &mut tier4] {
        v.sort_unstable();
    }
    if !tier1.is_empty() {
        return tier1;
    }
    if !tier2.is_empty() {
        return tier2;
    }
    if !tier3.is_empty() {
        return tier3;
    }
    tier4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use crate::storage::Storage;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn sym(qname: &str, name: &str, parent: Option<&str>) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: "function".into(),
            start_line: 1,
            start_col: 0,
            end_line: 1,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 0,
            signature: None,
            visibility: None,
            parent_qualified_name: parent.map(str::to_string),
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

    fn id_of(g: &Graph, qname: &str) -> i64 {
        *g.symbols
            .iter()
            .find(|(_, m)| m.qualified_name == qname)
            .unwrap_or_else(|| panic!("no symbol with qname {qname}"))
            .0
    }

    #[test]
    fn test_path_self_to_self_distance_zero() {
        let (_g, mut s) = tmp_storage();
        let ef = file("a.rs", vec![sym("only", "only", None)]);
        insert_extracted_files(&mut s, &[ef]).unwrap();
        let g = Graph::load(&s).unwrap();
        let id = id_of(&g, "only");
        let p = shortest_path(&g, id, id).unwrap().unwrap();
        assert_eq!(p.distance, 0);
        assert_eq!(p.nodes.len(), 1);
        assert!(p.edges.is_empty());
    }

    #[test]
    fn test_path_direct_call_edge_distance_one() {
        let (_g, mut s) = tmp_storage();
        let mut a = file("a.rs", vec![sym("caller", "caller", None)]);
        a.calls.push(ExtractedCall {
            caller_qualified_name: "caller".into(),
            callee_raw_name: "callee".into(),
            line: 1,
            col: 0,
        });
        let b = file("b.rs", vec![sym("callee", "callee", None)]);
        insert_extracted_files(&mut s, &[a, b]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let g = Graph::load(&s).unwrap();
        let from = id_of(&g, "caller");
        let to = id_of(&g, "callee");
        let p = shortest_path(&g, from, to).unwrap().unwrap();
        assert_eq!(p.distance, 1);
        assert_eq!(p.nodes.len(), 2);
        assert_eq!(p.edges.len(), 1);
        assert_eq!(p.nodes[0].name, "caller");
        assert_eq!(p.nodes[1].name, "callee");
        assert_eq!(p.edges[0].kind, EdgeKind::Call);
    }

    #[test]
    fn test_path_two_hops_through_call_chain() {
        let (_g, mut s) = tmp_storage();
        // a → b → c
        let mut a = file("a.rs", vec![sym("a_fn", "a_fn", None)]);
        a.calls.push(ExtractedCall {
            caller_qualified_name: "a_fn".into(),
            callee_raw_name: "b_fn".into(),
            line: 1, col: 0,
        });
        let mut b = file("b.rs", vec![sym("b_fn", "b_fn", None)]);
        b.calls.push(ExtractedCall {
            caller_qualified_name: "b_fn".into(),
            callee_raw_name: "c_fn".into(),
            line: 1, col: 0,
        });
        let c = file("c.rs", vec![sym("c_fn", "c_fn", None)]);
        insert_extracted_files(&mut s, &[a, b, c]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let g = Graph::load(&s).unwrap();
        let from = id_of(&g, "a_fn");
        let to = id_of(&g, "c_fn");
        let p = shortest_path(&g, from, to).unwrap().unwrap();
        assert_eq!(p.distance, 2);
        assert_eq!(p.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
                   vec!["a_fn", "b_fn", "c_fn"]);
    }

    #[test]
    fn test_path_disconnected_returns_none() {
        let (_g, mut s) = tmp_storage();
        let a = file("a.rs", vec![sym("alone", "alone", None)]);
        let b = file("b.rs", vec![sym("apart", "apart", None)]);
        insert_extracted_files(&mut s, &[a, b]).unwrap();
        let g = Graph::load(&s).unwrap();
        let from = id_of(&g, "alone");
        let to = id_of(&g, "apart");
        let p = shortest_path(&g, from, to).unwrap();
        assert!(p.is_none());
    }

    #[test]
    fn test_path_unknown_id_returns_none() {
        let (_g, mut s) = tmp_storage();
        let a = file("a.rs", vec![sym("only", "only", None)]);
        insert_extracted_files(&mut s, &[a]).unwrap();
        let g = Graph::load(&s).unwrap();
        let id = id_of(&g, "only");
        // 99999 is not in the graph.
        assert!(shortest_path(&g, id, 99999).unwrap().is_none());
        assert!(shortest_path(&g, 99999, id).unwrap().is_none());
    }

    #[test]
    fn test_resolve_symbol_exact_qname_wins() {
        let (_g, mut s) = tmp_storage();
        let f = file(
            "a.rs",
            vec![
                sym("foo::bar", "bar", None),
                sym("foo::bar::nested", "nested", Some("foo::bar")),
            ],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let g = Graph::load(&s).unwrap();
        let ids = resolve_symbol_to_id(&g, "foo::bar");
        assert_eq!(ids.len(), 1);
        let qname = &g.symbols[&ids[0]].qualified_name;
        assert_eq!(qname, "foo::bar");
    }

    #[test]
    fn test_resolve_symbol_exact_name_when_no_qname_match() {
        let (_g, mut s) = tmp_storage();
        let f = file("a.rs", vec![sym("crate::a::helper", "helper", None)]);
        insert_extracted_files(&mut s, &[f]).unwrap();
        let g = Graph::load(&s).unwrap();
        let ids = resolve_symbol_to_id(&g, "helper");
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn test_resolve_symbol_qname_ends_with_query() {
        // User typed `bar::nested` — should match `foo::bar::nested`.
        let (_g, mut s) = tmp_storage();
        let f = file(
            "a.rs",
            vec![sym("foo::bar::nested", "nested", None)],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let g = Graph::load(&s).unwrap();
        let ids = resolve_symbol_to_id(&g, "bar::nested");
        assert_eq!(ids.len(), 1);
        assert_eq!(g.symbols[&ids[0]].qualified_name, "foo::bar::nested");
    }

    #[test]
    fn test_resolve_symbol_returns_multiple_candidates_for_ambiguous_name() {
        let (_g, mut s) = tmp_storage();
        // Two files, two symbols both named "Helper" with different qnames.
        let a = file("a.rs", vec![sym("a::Helper", "Helper", None)]);
        let b = file("b.rs", vec![sym("b::Helper", "Helper", None)]);
        insert_extracted_files(&mut s, &[a, b]).unwrap();
        let g = Graph::load(&s).unwrap();
        let ids = resolve_symbol_to_id(&g, "Helper");
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_resolve_symbol_empty_query_returns_empty() {
        let (_g, mut s) = tmp_storage();
        let f = file("a.rs", vec![sym("only", "only", None)]);
        insert_extracted_files(&mut s, &[f]).unwrap();
        let g = Graph::load(&s).unwrap();
        let ids = resolve_symbol_to_id(&g, "");
        assert!(ids.is_empty());
        let ids = resolve_symbol_to_id(&g, "   ");
        assert!(ids.is_empty());
    }

    #[test]
    fn test_path_via_parent_edge() {
        let (_g, mut s) = tmp_storage();
        let parent = sym("Owner", "Owner", None);
        let child = sym("Owner::method", "method", Some("Owner"));
        let f = file("a.rs", vec![parent, child]);
        insert_extracted_files(&mut s, &[f]).unwrap();
        let g = Graph::load(&s).unwrap();
        let from = id_of(&g, "Owner");
        let to = id_of(&g, "Owner::method");
        let p = shortest_path(&g, from, to).unwrap().unwrap();
        assert_eq!(p.distance, 1);
        assert_eq!(p.edges[0].kind, EdgeKind::Parent);
    }
    #[test]
    fn test_resolve_symbol_candidates_matches_graph_tiers() {
        let (_g, mut s) = tmp_storage();
        let f = file(
            "a.rs",
            vec![
                sym("crate::foo::bar", "bar", None),
                sym("crate::baz::bar", "bar", None),
                sym("crate::foobar", "foobar", None),
            ],
        );
        insert_extracted_files(&mut s, &[f]).unwrap();
        let graph = Graph::load(&s).unwrap();
        for q in ["crate::foo::bar", "bar", "foo::bar", "oob", "nothing_here", ""] {
            let via_graph = resolve_symbol_to_id(&graph, q);
            let via_sql: Vec<i64> = resolve_symbol_candidates(&s, q).unwrap().iter().map(|c| c.symbol_id).collect();
            assert_eq!(via_graph, via_sql, "tier mismatch for query {q:?}");
        }
        let c = resolve_symbol_candidates(&s, "crate::foo::bar").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].language, "rust");
        assert_eq!(c[0].file_path, "a.rs");
    }

    #[test]
    fn test_resolve_symbol_candidates_underscore_is_literal_not_wildcard() {
        let (_g, mut s) = tmp_storage();
        let f = file("a.py", vec![sym("m.a_b", "a_b", None), sym("m.axb", "axb", None)]);
        insert_extracted_files(&mut s, &[f]).unwrap();
        let c = resolve_symbol_candidates(&s, "m.a_b").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "a_b");
    }
}
