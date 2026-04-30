//! Plain-language explanation of a symbol and its neighbors —
//! graphify-parity for `graphify explain "X"`.
//!
//! Builds on [`crate::query::Graph`]. For a focus symbol, gathers all its
//! direct neighbors (one hop) bucketed by [`crate::query::EdgeKind`].
//! Output is consumed by `lens-cli::cmd::explain::render_markdown` to
//! produce the human-readable card.

use crate::error::Result;
use crate::query::{EdgeKind, Graph, QueryNode};

/// Direct-neighbor breakdown of a focused symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainResult {
    pub focus: QueryNode,
    pub parents: Vec<QueryNode>,
    pub children: Vec<QueryNode>,
    pub callers: Vec<QueryNode>,
    pub callees: Vec<QueryNode>,
    pub types: Vec<QueryNode>,
    pub imports: Vec<QueryNode>,
}

impl ExplainResult {
    /// True when the focus has no neighbors of any kind.
    pub fn is_isolated(&self) -> bool {
        self.parents.is_empty()
            && self.children.is_empty()
            && self.callers.is_empty()
            && self.callees.is_empty()
            && self.types.is_empty()
            && self.imports.is_empty()
    }

    /// Total neighbor count.
    pub fn total_neighbors(&self) -> usize {
        self.parents.len()
            + self.children.len()
            + self.callers.len()
            + self.callees.len()
            + self.types.len()
            + self.imports.len()
    }
}

/// Build an [`ExplainResult`] for symbol `sid` over `graph`.
///
/// `Ok(None)` when `sid` is not present in the graph.
pub fn explain_symbol(graph: &Graph, sid: i64) -> Result<Option<ExplainResult>> {
    let Some(meta) = graph.symbols.get(&sid) else {
        return Ok(None);
    };
    let focus = QueryNode {
        symbol_id: sid,
        qualified_name: meta.qualified_name.clone(),
        name: meta.name.clone(),
        kind: meta.kind.clone(),
        file_path: meta.file_path.clone(),
        start_line: meta.start_line,
        is_seed: true,
    };

    let mut result = ExplainResult {
        focus,
        parents: Vec::new(),
        children: Vec::new(),
        callers: Vec::new(),
        callees: Vec::new(),
        types: Vec::new(),
        imports: Vec::new(),
    };

    // Parent vs child split: walk parent_symbol_id chain in `graph.symbols`
    // (which preserved the row-level data via Graph::load's parent edge).
    // The Parent edge in adjacency is undirected; to distinguish "this is my
    // parent" vs "this is my child" we re-consult the symbols map.
    if let Some(neighbors) = graph.adjacency.get(&sid) {
        let mut seen_neighbors: std::collections::HashSet<(i64, EdgeKind)> =
            std::collections::HashSet::new();
        for &(nbr, kind) in neighbors {
            if !seen_neighbors.insert((nbr, kind)) {
                continue; // dedup duplicate parallel edges
            }
            let Some(nbr_meta) = graph.symbols.get(&nbr) else {
                continue;
            };
            let node = QueryNode {
                symbol_id: nbr,
                qualified_name: nbr_meta.qualified_name.clone(),
                name: nbr_meta.name.clone(),
                kind: nbr_meta.kind.clone(),
                file_path: nbr_meta.file_path.clone(),
                start_line: nbr_meta.start_line,
                is_seed: false,
            };
            match kind {
                EdgeKind::Parent => {
                    // The neighbor is either my parent (their qname is a
                    // strict prefix of mine using the language's separator)
                    // or my child (mine is a strict prefix of theirs).
                    if is_qname_prefix_of(&result.focus.qualified_name, &nbr_meta.qualified_name) {
                        result.children.push(node);
                    } else if is_qname_prefix_of(
                        &nbr_meta.qualified_name,
                        &result.focus.qualified_name,
                    ) {
                        result.parents.push(node);
                    } else {
                        // Neither prefix — non-hierarchical Parent edge. Bucket
                        // as parent for display; should be rare given how
                        // Graph::load constructs Parent edges from
                        // parent_symbol_id (always strict containment).
                        result.parents.push(node);
                    }
                }
                EdgeKind::Call => {
                    // Heuristic: any Call edge between me and a neighbor
                    // could be inbound or outbound. Without preserving
                    // directionality in adjacency (Graph stores edges as
                    // undirected pairs), we cannot distinguish caller vs
                    // callee here cheaply. v1: bucket all Call edges as
                    // "callees" — semantically "things I'm related to via
                    // calls." A future refinement can split inbound/outbound
                    // by re-querying the calls table or by storing
                    // directionality in the adjacency.
                    result.callees.push(node);
                }
                EdgeKind::Type => result.types.push(node),
                EdgeKind::Import => result.imports.push(node),
            }
        }
    }

    // Sort each bucket by qualified_name for deterministic output.
    for v in [
        &mut result.parents,
        &mut result.children,
        &mut result.callers,
        &mut result.callees,
        &mut result.types,
        &mut result.imports,
    ] {
        v.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    }

    Ok(Some(result))
}

/// True when `prefix` is a strict qualified-name prefix of `full`. Handles
/// both Rust (`::`) and Python (`.`) separators since the qname is opaque
/// per the per-language convention. Strict means `prefix != full`.
fn is_qname_prefix_of(prefix: &str, full: &str) -> bool {
    if prefix.is_empty() || prefix == full {
        return false;
    }
    if !full.starts_with(prefix) {
        return false;
    }
    let rest = &full[prefix.len()..];
    rest.starts_with("::") || rest.starts_with('.')
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
        *g.symbols.iter().find(|(_, m)| m.qualified_name == qname).unwrap().0
    }

    #[test]
    fn test_explain_symbol_unknown_id_returns_none() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(&mut s, &[file("a.rs", vec![sym("only", "only", None)])]).unwrap();
        let g = Graph::load(&s).unwrap();
        let r = explain_symbol(&g, 99999).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_explain_symbol_isolated_node_has_no_neighbors() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(&mut s, &[file("a.rs", vec![sym("alone", "alone", None)])]).unwrap();
        let g = Graph::load(&s).unwrap();
        let id = id_of(&g, "alone");
        let r = explain_symbol(&g, id).unwrap().unwrap();
        assert_eq!(r.focus.name, "alone");
        assert!(r.is_isolated());
        assert_eq!(r.total_neighbors(), 0);
    }

    #[test]
    fn test_explain_symbol_parent_child_buckets_correct() {
        let (_g, mut s) = tmp_storage();
        let parent = sym("Owner", "Owner", None);
        let child = sym("Owner::method", "method", Some("Owner"));
        insert_extracted_files(&mut s, &[file("a.rs", vec![parent, child])]).unwrap();
        let g = Graph::load(&s).unwrap();
        let owner = id_of(&g, "Owner");
        let method = id_of(&g, "Owner::method");

        // From owner's perspective, method is a child.
        let r1 = explain_symbol(&g, owner).unwrap().unwrap();
        assert_eq!(r1.children.len(), 1);
        assert_eq!(r1.children[0].name, "method");
        assert!(r1.parents.is_empty());

        // From method's perspective, owner is a parent.
        let r2 = explain_symbol(&g, method).unwrap().unwrap();
        assert_eq!(r2.parents.len(), 1);
        assert_eq!(r2.parents[0].name, "Owner");
        assert!(r2.children.is_empty());
    }

    #[test]
    fn test_explain_symbol_call_neighbors_in_callees_bucket() {
        let (_g, mut s) = tmp_storage();
        let mut a = file("a.rs", vec![sym("caller", "caller", None)]);
        a.calls.push(ExtractedCall {
            caller_qualified_name: "caller".into(),
            callee_raw_name: "callee".into(),
            line: 1, col: 0,
        });
        let b = file("b.rs", vec![sym("callee", "callee", None)]);
        insert_extracted_files(&mut s, &[a, b]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        let g = Graph::load(&s).unwrap();
        let caller = id_of(&g, "caller");
        let r = explain_symbol(&g, caller).unwrap().unwrap();
        assert_eq!(r.callees.len(), 1);
        assert_eq!(r.callees[0].name, "callee");
    }

    #[test]
    fn test_explain_result_default_neighbor_buckets_sorted() {
        // Build a focus with multiple neighbors of one kind; assert each
        // bucket sorts by qualified_name.
        let (_g, mut s) = tmp_storage();
        let parent = sym("P", "P", None);
        let child_b = sym("P::b_child", "b_child", Some("P"));
        let child_a = sym("P::a_child", "a_child", Some("P"));
        insert_extracted_files(&mut s, &[file("a.rs", vec![parent, child_b, child_a])])
            .unwrap();
        let g = Graph::load(&s).unwrap();
        let p_id = id_of(&g, "P");
        let r = explain_symbol(&g, p_id).unwrap().unwrap();
        assert_eq!(r.children.len(), 2);
        assert_eq!(r.children[0].qualified_name, "P::a_child");
        assert_eq!(r.children[1].qualified_name, "P::b_child");
    }

    #[test]
    fn test_is_qname_prefix_of_handles_rust_separator() {
        assert!(is_qname_prefix_of("foo", "foo::bar"));
        assert!(is_qname_prefix_of("foo::bar", "foo::bar::baz"));
        assert!(!is_qname_prefix_of("foo", "foobar"));
        assert!(!is_qname_prefix_of("foo", "foo"));
        assert!(!is_qname_prefix_of("", "foo"));
    }

    #[test]
    fn test_is_qname_prefix_of_handles_python_separator() {
        assert!(is_qname_prefix_of("pkg", "pkg.module"));
        assert!(is_qname_prefix_of("pkg.module", "pkg.module.Class"));
        assert!(!is_qname_prefix_of("pkg", "pkgmodule"));
    }
}
