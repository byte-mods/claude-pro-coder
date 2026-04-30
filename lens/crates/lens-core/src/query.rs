//! Question-driven graph traversal — the "graphify query" parity primitive.
//!
//! Loads the symbol graph from SQLite into an adjacency list once per call,
//! seeds traversal from symbols whose name or qualified_name matches tokens
//! pulled from the question, and expands BFS or DFS up to a token budget.
//!
//! ## Why an in-memory adjacency
//!
//! The graph fits in memory for any project that fits in a developer's
//! mental model (~10K-100K symbols). Building it once and traversing in RAM
//! is dramatically faster than running a fresh JOIN per BFS frontier
//! expansion. The cost is one full-table scan per `query_graph` call —
//! acceptable for an interactive CLI; if it ever bites, cache the graph
//! across calls inside a long-running process.
//!
//! ## Edge model
//!
//! Symbols are nodes. Edges loaded into the graph (matching the [`EdgeKind`]
//! enum):
//!
//! - `Parent` — `symbols.parent_symbol_id` (child ↔ parent).
//! - `Call` — `calls.caller_symbol_id ↔ calls.callee_symbol_id` (only when
//!   both ends are resolved; unresolved callees are dropped).
//! - `Type` — `types.symbol_id ↔ types.target_symbol_id` (when both ends
//!   are resolved).
//! - `Import` — projected from a file-level row (`imports.resolved_symbol_id`
//!   linked from `imports.file_id`) onto the symbol graph: every top-level
//!   symbol in the importing file is connected to the resolved symbol via
//!   the `file_top_symbols` map.
//!
//! Raw `refs` rows are NOT loaded as a separate edge kind — a `ref` to a
//! symbol that has an associated `import` already produces an edge through
//! the import projection above. A future T-task may add an `EdgeKind::Ref`
//! if direct ref-as-edge semantics are needed (e.g. for `lens follow`).
//!
//! All edges are treated as undirected for traversal (caller of, called by,
//! parent of, child of all reachable). The `EdgeKind` is preserved so the
//! formatter can label edges.
//!
//! ## Seeding
//!
//! The question is tokenised on whitespace and `.,?!:;`. Tokens shorter than
//! 3 chars and a small stopword list are dropped. Each remaining token is
//! matched against `symbols.name` (substring, case-insensitive) and
//! `symbols.qualified_name` (substring, case-insensitive). Matches across
//! tokens are unioned. Up to `MAX_SEEDS` (16) seed nodes are kept; ties are
//! broken by exact-match-first, then shortest-name-first (less ambiguous).
//!
//! ## Traversal & budget
//!
//! BFS by default, DFS optional. Each visited symbol contributes ~`AVG_NODE_TOKENS`
//! to the running budget estimate (a heuristic — actual rendered tokens
//! depend on the formatter). Traversal stops when adding one more node
//! would exceed `budget_tokens`. The seed nodes are always included even
//! if their cumulative size already exceeds the budget — otherwise an
//! over-tight budget would yield empty output.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::error::{LensError, Result};
use crate::storage::Storage;

/// Average tokens contributed per visited symbol when rendered. Tunable;
/// raise to truncate harder, lower to fit more nodes per budget.
pub const AVG_NODE_TOKENS: u32 = 80;

/// Hard cap on seed nodes per question. Prevents a vague question from
/// fanning out into thousands of low-relevance matches.
pub const MAX_SEEDS: usize = 16;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "what", "which", "where", "how", "does", "from", "into", "with", "this",
    "that", "are", "was", "were", "have", "has", "had", "but", "all", "you", "can", "could",
    "should", "would", "will", "not", "any", "let", "set", "get", "use", "uses", "used", "show",
    "find", "tell",
];

/// Traversal mode. Mirrors graphify's `--dfs` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalMode {
    Bfs,
    Dfs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    Parent,
    Call,
    Type,
    Import,
}

/// One node in the query result — a symbol with its location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryNode {
    pub symbol_id: i64,
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: i64,
    /// True if this node was a seed (matched the question directly).
    pub is_seed: bool,
}

/// One edge in the query result — labelled by kind, undirected for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryEdge {
    pub from_symbol_id: i64,
    pub to_symbol_id: i64,
    pub kind: EdgeKind,
}

/// Result of a graph traversal driven by a free-text question.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub seeds: Vec<i64>,
    pub nodes: Vec<QueryNode>,
    pub edges: Vec<QueryEdge>,
    pub estimated_tokens: u32,
    pub mode: Option<TraversalMode>,
    pub budget: u32,
}

/// Run a question-driven traversal over the symbol graph in `storage`.
///
/// Returns an empty result (no nodes, no edges) if no symbols match the
/// question's tokens. Errors only on storage access failure, never on
/// "couldn't find anything to say."
pub fn query_graph(
    storage: &Storage,
    question: &str,
    mode: TraversalMode,
    budget_tokens: u32,
) -> Result<QueryResult> {
    let graph = Graph::load(storage)?;
    let seeds = seed_nodes_from_question(&graph, question);

    let mut result = QueryResult {
        seeds: seeds.clone(),
        mode: Some(mode),
        budget: budget_tokens,
        ..Default::default()
    };

    if seeds.is_empty() {
        return Ok(result);
    }

    // Visited set keyed by symbol_id. Seeds go in first.
    let mut visited: HashSet<i64> = HashSet::new();
    let mut traversed_edges: BTreeSet<(i64, i64, EdgeKind)> = BTreeSet::new();

    // Budget accounting — seeds are always admitted regardless of budget so
    // the user gets *something* back even with a tiny budget.
    let mut budget_used: u32 = 0;
    for &sid in &seeds {
        if visited.insert(sid) {
            push_node(&graph, sid, true, &mut result.nodes);
            budget_used = budget_used.saturating_add(AVG_NODE_TOKENS);
        }
    }

    // Frontier expansion: BFS via VecDeque, DFS via Vec-as-stack.
    let mut bfs_q: VecDeque<i64> = VecDeque::new();
    let mut dfs_q: Vec<i64> = Vec::new();
    for &sid in &seeds {
        match mode {
            TraversalMode::Bfs => bfs_q.push_back(sid),
            TraversalMode::Dfs => dfs_q.push(sid),
        }
    }

    loop {
        let current = match mode {
            TraversalMode::Bfs => bfs_q.pop_front(),
            TraversalMode::Dfs => dfs_q.pop(),
        };
        let Some(current) = current else { break };

        // Stop expanding once the budget would be blown by even one more
        // node. We intentionally check BEFORE pushing the next round so the
        // result stays under the cap.
        if budget_used.saturating_add(AVG_NODE_TOKENS) > budget_tokens {
            break;
        }

        let neighbors = match graph.adjacency.get(&current) {
            Some(v) => v.clone(),
            None => continue,
        };

        for (nbr, kind) in neighbors {
            // Record the edge regardless of visited-ness (so `nodes`
            // already in the result get edges to newly-found ones).
            let canon = if current < nbr {
                (current, nbr, kind)
            } else {
                (nbr, current, kind)
            };
            if traversed_edges.insert(canon) {
                result.edges.push(QueryEdge {
                    from_symbol_id: canon.0,
                    to_symbol_id: canon.1,
                    kind: canon.2,
                });
            }

            if visited.insert(nbr) {
                if budget_used.saturating_add(AVG_NODE_TOKENS) > budget_tokens {
                    // Frontier ran out — undo the insert? No: visited is
                    // the dedup set; we just don't push a node for this
                    // symbol. It's still considered "seen" so we don't add
                    // it again.
                    visited.remove(&nbr);
                    break;
                }
                push_node(&graph, nbr, false, &mut result.nodes);
                budget_used = budget_used.saturating_add(AVG_NODE_TOKENS);
                match mode {
                    TraversalMode::Bfs => bfs_q.push_back(nbr),
                    TraversalMode::Dfs => dfs_q.push(nbr),
                }
            }
        }
    }

    result.estimated_tokens = budget_used;
    Ok(result)
}

fn push_node(graph: &Graph, sid: i64, is_seed: bool, out: &mut Vec<QueryNode>) {
    if let Some(meta) = graph.symbols.get(&sid) {
        out.push(QueryNode {
            symbol_id: sid,
            qualified_name: meta.qualified_name.clone(),
            name: meta.name.clone(),
            kind: meta.kind.clone(),
            file_path: meta.file_path.clone(),
            start_line: meta.start_line,
            is_seed,
        });
    }
}

/// Tokenise the question and pick the strongest seed symbols. Public for
/// reuse by other commands (e.g. `lens explain` could use the same logic).
pub fn seed_nodes_from_question(graph: &Graph, question: &str) -> Vec<i64> {
    let tokens = tokenise(question);
    if tokens.is_empty() {
        return Vec::new();
    }

    // Score each symbol by how many tokens it matches and how cleanly.
    // Exact-name match scores higher than substring-of-qname.
    let mut scored: Vec<(i64, u32, usize)> = Vec::new();
    for (sid, meta) in &graph.symbols {
        let mut score: u32 = 0;
        let name_lc = meta.name.to_ascii_lowercase();
        let qname_lc = meta.qualified_name.to_ascii_lowercase();
        for tok in &tokens {
            if name_lc == *tok {
                score += 100;
            } else if name_lc.contains(tok.as_str()) {
                score += 30;
            }
            if qname_lc.contains(tok.as_str()) {
                score += 10;
            }
        }
        if score > 0 {
            scored.push((*sid, score, meta.name.len()));
        }
    }

    // Highest score wins; ties broken by shorter name (less likely to be a
    // module/path); final tiebreak by symbol_id for determinism.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
    scored.truncate(MAX_SEEDS);
    scored.into_iter().map(|(sid, _, _)| sid).collect()
}

fn tokenise(question: &str) -> Vec<String> {
    let lower = question.to_ascii_lowercase();
    let mut out = Vec::new();
    for raw in lower.split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':' && c != '.') {
        let tok = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string();
        if tok.len() < 3 {
            continue;
        }
        if STOPWORDS.contains(&tok.as_str()) {
            continue;
        }
        if !out.contains(&tok) {
            out.push(tok);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct SymbolMeta {
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file_id: i64,
    pub file_path: String,
    pub start_line: i64,
    /// Language of the symbol's source file (matches `LanguageId::as_str()`):
    /// "rust", "python", "typescript", "javascript", "go". Used to surface
    /// cross-language matches in disambiguation messages so Claude can tell
    /// when `Foo` resolved to a Python class vs a Rust struct.
    pub language: String,
}

/// Loaded symbol graph — symbols + adjacency + file→top-symbol map for
/// projecting file-level edges (refs, imports) onto the symbol graph.
#[derive(Debug, Default)]
pub struct Graph {
    pub symbols: HashMap<i64, SymbolMeta>,
    pub adjacency: HashMap<i64, Vec<(i64, EdgeKind)>>,
    /// file_id → top-level symbols in that file (used to project
    /// file-level refs/imports onto the symbol graph).
    pub file_top_symbols: HashMap<i64, Vec<i64>>,
}

impl Graph {
    /// Load the full symbol graph from `storage` into memory. One scan per
    /// table; complexity O(symbols + calls + types + refs + imports).
    pub fn load(storage: &Storage) -> Result<Self> {
        let conn = storage.connection();
        let mut graph = Graph::default();

        // Symbols + their file paths (for display) + file's language (for
        // cross-language disambiguation).
        {
            let mut stmt = conn
                .prepare(
                    "SELECT s.id, s.qualified_name, s.name, s.kind, s.file_id, f.path, s.start_line, s.parent_symbol_id, f.language
                     FROM symbols s
                     JOIN files f ON f.id = s.file_id",
                )
                .map_err(|e| LensError::other(format!("prepare load symbols: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                })
                .map_err(|e| LensError::other(format!("query load symbols: {e}")))?;
            for r in rows {
                let (id, qname, name, kind, file_id, file_path, start_line, parent_id, language) =
                    r.map_err(|e| LensError::other(format!("row load symbols: {e}")))?;
                graph.symbols.insert(
                    id,
                    SymbolMeta {
                        qualified_name: qname,
                        name,
                        kind,
                        file_id,
                        file_path,
                        start_line,
                        language,
                    },
                );
                if parent_id.is_none() {
                    graph.file_top_symbols.entry(file_id).or_default().push(id);
                } else if let Some(p) = parent_id {
                    add_edge(&mut graph.adjacency, id, p, EdgeKind::Parent);
                }
            }
        }

        // Calls (only resolved → resolved).
        {
            let mut stmt = conn
                .prepare(
                    "SELECT caller_symbol_id, callee_symbol_id FROM calls
                     WHERE callee_symbol_id IS NOT NULL",
                )
                .map_err(|e| LensError::other(format!("prepare load calls: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| LensError::other(format!("query load calls: {e}")))?;
            for r in rows {
                let (caller, callee) =
                    r.map_err(|e| LensError::other(format!("row load calls: {e}")))?;
                add_edge(&mut graph.adjacency, caller, callee, EdgeKind::Call);
            }
        }

        // Type relations.
        {
            let mut stmt = conn
                .prepare(
                    "SELECT symbol_id, target_symbol_id FROM types
                     WHERE target_symbol_id IS NOT NULL",
                )
                .map_err(|e| LensError::other(format!("prepare load types: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| LensError::other(format!("query load types: {e}")))?;
            for r in rows {
                let (sid, tid) =
                    r.map_err(|e| LensError::other(format!("row load types: {e}")))?;
                add_edge(&mut graph.adjacency, sid, tid, EdgeKind::Type);
            }
        }

        // Imports — file-level. Project each import to "every top-level
        // symbol in the file ↔ resolved symbol".
        {
            let mut stmt = conn
                .prepare(
                    "SELECT file_id, resolved_symbol_id FROM imports
                     WHERE resolved_symbol_id IS NOT NULL",
                )
                .map_err(|e| LensError::other(format!("prepare load imports: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|e| LensError::other(format!("query load imports: {e}")))?;
            let imports: Vec<(i64, i64)> = rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| LensError::other(format!("collect imports: {e}")))?;
            for (file_id, resolved_sym) in imports {
                if let Some(top_syms) = graph.file_top_symbols.get(&file_id) {
                    for &top in top_syms {
                        add_edge(&mut graph.adjacency, top, resolved_sym, EdgeKind::Import);
                    }
                }
            }
        }

        Ok(graph)
    }
}

fn add_edge(
    adj: &mut HashMap<i64, Vec<(i64, EdgeKind)>>,
    a: i64,
    b: i64,
    kind: EdgeKind,
) {
    if a == b {
        return; // self-edges add no information for traversal.
    }
    adj.entry(a).or_default().push((b, kind));
    adj.entry(b).or_default().push((a, kind));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedSymbol};
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn make_symbol(qname: &str, name: &str, parent: Option<&str>) -> ExtractedSymbol {
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

    fn rust_file(path: &str, syms: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Rust);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 100;
        ef.modified_at = 1;
        ef.symbols = syms;
        ef
    }

    fn seed_two_file_call_graph() -> (TempDir, Storage) {
        // a.rs: pub fn caller() { callee() }
        // b.rs: pub fn callee() {}
        let (g, mut s) = tmp_storage();
        let mut a = rust_file("a.rs", vec![make_symbol("caller", "caller", None)]);
        a.calls.push(ExtractedCall {
            caller_qualified_name: "caller".into(),
            callee_raw_name: "callee".into(),
            line: 1,
            col: 0,
        });
        let b = rust_file("b.rs", vec![make_symbol("callee", "callee", None)]);
        insert_extracted_files(&mut s, &[a, b]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        (g, s)
    }

    #[test]
    fn test_query_graph_seeds_from_exact_name_match() {
        let (_g, s) = seed_two_file_call_graph();
        let r = query_graph(&s, "find callee", TraversalMode::Bfs, 5000).unwrap();
        assert!(!r.seeds.is_empty(), "expected at least one seed");
        let seed_qname: String = s
            .connection()
            .query_row(
                "SELECT qualified_name FROM symbols WHERE id = ?1",
                rusqlite::params![r.seeds[0]],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(seed_qname, "callee");
    }

    #[test]
    fn test_query_graph_bfs_expands_through_call_edge() {
        let (_g, s) = seed_two_file_call_graph();
        let r = query_graph(&s, "callee", TraversalMode::Bfs, 5000).unwrap();
        // Both 'callee' (seed) and 'caller' (call edge) must be in nodes.
        let names: Vec<&str> = r.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"callee"));
        assert!(names.contains(&"caller"));
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Call));
    }

    #[test]
    fn test_query_graph_dfs_returns_same_set_smaller_graph() {
        let (_g, s) = seed_two_file_call_graph();
        let bfs = query_graph(&s, "callee", TraversalMode::Bfs, 5000).unwrap();
        let dfs = query_graph(&s, "callee", TraversalMode::Dfs, 5000).unwrap();
        let bfs_ids: HashSet<i64> = bfs.nodes.iter().map(|n| n.symbol_id).collect();
        let dfs_ids: HashSet<i64> = dfs.nodes.iter().map(|n| n.symbol_id).collect();
        // For a graph this small, BFS and DFS must visit the same set.
        assert_eq!(bfs_ids, dfs_ids);
    }

    #[test]
    fn test_query_graph_no_match_returns_empty_with_no_error() {
        let (_g, s) = seed_two_file_call_graph();
        let r = query_graph(&s, "zzznotapresentword", TraversalMode::Bfs, 5000).unwrap();
        assert!(r.seeds.is_empty());
        assert!(r.nodes.is_empty());
        assert!(r.edges.is_empty());
    }

    #[test]
    fn test_query_graph_seeds_always_included_under_tight_budget() {
        let (_g, s) = seed_two_file_call_graph();
        // Budget 0 — seeds still get included so the user sees something.
        let r = query_graph(&s, "callee", TraversalMode::Bfs, 0).unwrap();
        assert!(!r.nodes.is_empty(), "seeds must always be included");
        assert!(r.nodes.iter().all(|n| n.is_seed), "no expansion on budget=0");
    }

    #[test]
    fn test_query_graph_respects_token_budget() {
        // Build a star: center is connected to N leaves. Tight budget should
        // truncate before all leaves arrive.
        let (_g, mut s) = tmp_storage();
        let mut center = rust_file("c.rs", vec![make_symbol("center", "center", None)]);
        // Add many calls to leaves in different files.
        let mut files: Vec<ExtractedFile> = Vec::new();
        for i in 0..10 {
            let leaf_qname = format!("leaf{i}");
            let leaf_file = rust_file(
                &format!("l{i}.rs"),
                vec![make_symbol(&leaf_qname, &leaf_qname, None)],
            );
            files.push(leaf_file);
            center.calls.push(ExtractedCall {
                caller_qualified_name: "center".into(),
                callee_raw_name: leaf_qname,
                line: 1,
                col: 0,
            });
        }
        files.insert(0, center);
        insert_extracted_files(&mut s, &files).unwrap();
        resolve_cross_file_references(&mut s).unwrap();

        // 1 seed (~80 tokens) + budget = 200 → can fit at most 2 more.
        let r = query_graph(&s, "center", TraversalMode::Bfs, 200).unwrap();
        assert!(r.nodes.len() <= 4, "tight budget must truncate; got {}", r.nodes.len());
        assert!(!r.nodes.is_empty());
    }

    #[test]
    fn test_query_graph_parent_edges_traversed() {
        // Parent symbol owns a child via parent_qualified_name.
        let (_g, mut s) = tmp_storage();
        let parent = make_symbol("Owner", "Owner", None);
        let child = make_symbol("Owner::method", "method", Some("Owner"));
        let ef = rust_file("a.rs", vec![parent, child]);
        insert_extracted_files(&mut s, &[ef]).unwrap();

        let r = query_graph(&s, "Owner", TraversalMode::Bfs, 5000).unwrap();
        let names: Vec<&str> = r.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Owner"));
        assert!(names.contains(&"method"));
        assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Parent));
    }

    #[test]
    fn test_query_graph_skips_unresolved_call_edges() {
        // Caller-with-unresolved-callee must not produce a fake edge.
        let (_g, mut s) = tmp_storage();
        let mut a = rust_file("a.rs", vec![make_symbol("caller", "caller", None)]);
        a.calls.push(ExtractedCall {
            caller_qualified_name: "caller".into(),
            callee_raw_name: "ghost_function".into(),
            line: 1,
            col: 0,
        });
        insert_extracted_files(&mut s, &[a]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();

        let r = query_graph(&s, "caller", TraversalMode::Bfs, 5000).unwrap();
        assert_eq!(r.nodes.len(), 1, "no expansion to ghost callee");
        assert!(r.edges.is_empty());
    }

    #[test]
    fn test_seed_nodes_filters_stopwords() {
        let (_g, s) = seed_two_file_call_graph();
        let g = Graph::load(&s).unwrap();
        // Only stopwords + short tokens.
        let seeds = seed_nodes_from_question(&g, "what does the");
        assert!(seeds.is_empty());
    }

    #[test]
    fn test_seed_nodes_caps_at_max_seeds() {
        // Build > MAX_SEEDS symbols whose names all contain "common".
        let (_g, mut s) = tmp_storage();
        let mut syms = Vec::new();
        for i in 0..(MAX_SEEDS + 5) {
            syms.push(make_symbol(
                &format!("common_{i}"),
                &format!("common_{i}"),
                None,
            ));
        }
        let ef = rust_file("a.rs", syms);
        insert_extracted_files(&mut s, &[ef]).unwrap();
        let g = Graph::load(&s).unwrap();
        let seeds = seed_nodes_from_question(&g, "common");
        assert_eq!(seeds.len(), MAX_SEEDS);
    }

    #[test]
    fn test_query_result_default_is_empty() {
        let r = QueryResult::default();
        assert!(r.seeds.is_empty());
        assert!(r.nodes.is_empty());
        assert!(r.edges.is_empty());
        assert_eq!(r.estimated_tokens, 0);
        assert_eq!(r.budget, 0);
        assert!(r.mode.is_none());
    }

    #[test]
    fn test_graph_load_self_edges_dropped() {
        // Synthesise a symbol that calls itself by manually inserting a call
        // row pointing to its own id.
        let (_g, mut s) = tmp_storage();
        let ef = rust_file("a.rs", vec![make_symbol("selfcaller", "selfcaller", None)]);
        insert_extracted_files(&mut s, &[ef]).unwrap();
        let sym_id: i64 = s
            .connection()
            .query_row(
                "SELECT id FROM symbols WHERE qualified_name = 'selfcaller'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let file_id: i64 = s
            .connection()
            .query_row("SELECT id FROM files", [], |r| r.get(0))
            .unwrap();
        s.connection()
            .execute(
                "INSERT INTO calls (caller_symbol_id, callee_symbol_id, callee_raw_name, file_id, line, col)
                 VALUES (?1, ?1, 'selfcaller', ?2, 1, 0)",
                rusqlite::params![sym_id, file_id],
            )
            .unwrap();

        let g = Graph::load(&s).unwrap();
        let neighbors = g.adjacency.get(&sym_id).cloned().unwrap_or_default();
        assert!(
            !neighbors.iter().any(|(n, _)| *n == sym_id),
            "self-edges must be dropped"
        );
    }
}
