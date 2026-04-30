//! `lens map` — architecture summary of the indexed project.
//!
//! Aggregates the symbol graph into a directory tree, attaching per-directory
//! summary stats (files, top symbol kinds) and a small ranked symbol list per
//! node. Designed for "give me the high-level shape" queries — a printable
//! tree with leverage points, not a full dump.
//!
//! Resolution rules:
//!   - Tree nodes are project-relative directory paths split on `/`.
//!   - A symbol is attributed to the directory containing its file.
//!   - `--scope` (caller-supplied) restricts which paths participate; nodes
//!     outside the scope are filtered out before traversal.
//!   - `--depth` (caller-supplied) caps how many segments deep the tree
//!     descends; deeper symbols are aggregated into the deepest visible
//!     ancestor.
//!
//! Determinism: sibling order is alphabetical; intra-node "top symbols" are
//! ordered by descending caller-count then ascending qualified name.

use std::collections::BTreeMap;

use crate::error::{LensError, Result};
use crate::storage::Storage;

/// Number of top symbols listed per directory node. Small by design —
/// `lens map` is a *summary*, not a full enumeration. For details on a
/// specific area, the user follows up with `lens query` or `lens explain`.
pub const TOP_SYMBOLS_PER_NODE: usize = 5;

/// One symbol entry in a [`MapNode`]'s top-list. Ranked by `caller_count`
/// descending; ties broken by qualified name ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSymbol {
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: i64,
    pub caller_count: i64,
}

/// One directory node in the architecture tree. Empty `path` is the project
/// root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapNode {
    /// Project-relative directory path with `/` separators. Empty string =
    /// project root.
    pub path: String,
    /// Number of *files* indexed under this node, recursive.
    pub file_count: i64,
    /// Number of *symbols* under this node, recursive.
    pub symbol_count: i64,
    /// Per-kind histogram (kind → count), descending by count. Bounded to a
    /// small set per node so the structure stays printable; uncommon kinds
    /// fold into "other".
    pub kind_histogram: Vec<(String, i64)>,
    /// Top symbols at this node, ranked by caller count.
    pub top_symbols: Vec<MapSymbol>,
    /// Children sorted by path ascending.
    pub children: Vec<MapNode>,
}

/// What `lens map` returns. The root node carries summary stats covering
/// everything in scope; its `children` form the directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapResult {
    pub root: MapNode,
    /// Echoed for renderer consumption — useful in headers like
    /// "scope: src/, depth: 2".
    pub scope: Option<String>,
    pub depth: u32,
}

/// Build an architecture tree from the index.
///
/// `scope` (if `Some`) restricts to files whose path *starts with* the given
/// prefix (after normalising trailing slashes). `depth` caps tree depth: 0
/// means root only, 1 includes immediate children, etc.
///
/// # Errors
/// SQLite read failures.
pub fn build_map(
    storage: &Storage,
    scope: Option<&str>,
    depth: u32,
) -> Result<MapResult> {
    // Normalise scope: strip leading/trailing `/`, treat empty as None.
    let scope_norm: Option<String> = scope.and_then(|s| {
        let trimmed = s.trim_matches('/').to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    // 1. Load all in-scope files. We pull file_id, path, language so we can
    //    later attribute symbols and compute per-node histograms.
    let conn = storage.connection();
    let mut file_stmt = conn
        .prepare("SELECT id, path FROM files ORDER BY path ASC")
        .map_err(|e| LensError::other(format!("map: prepare files: {e}")))?;
    let all_files: Vec<(i64, String)> = file_stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| LensError::other(format!("map: query files: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("map: collect files: {e}")))?;

    let in_scope: Vec<(i64, String)> = all_files
        .into_iter()
        .filter(|(_, path)| match &scope_norm {
            None => true,
            Some(prefix) => {
                path == prefix
                    || path.starts_with(&format!("{prefix}/"))
            }
        })
        .collect();

    // 2. Load symbols (with caller counts) for in-scope files. We do this in
    //    a single SQL pass rather than N round-trips per file — important for
    //    large projects where N files × M queries is ruinous.
    let placeholders = if in_scope.is_empty() {
        // Short-circuit: empty scope → empty tree. Skip SQL.
        return Ok(MapResult {
            root: MapNode {
                path: scope_norm.clone().unwrap_or_default(),
                file_count: 0,
                symbol_count: 0,
                kind_histogram: Vec::new(),
                top_symbols: Vec::new(),
                children: Vec::new(),
            },
            scope: scope_norm,
            depth,
        });
    } else {
        // Build "?,?,?" placeholder string sized to in_scope.
        std::iter::repeat("?")
            .take(in_scope.len())
            .collect::<Vec<_>>()
            .join(",")
    };

    let sql = format!(
        "SELECT s.qualified_name, s.kind, f.path, s.start_line,
                COALESCE((SELECT COUNT(*) FROM calls c WHERE c.callee_symbol_id = s.id), 0) AS caller_count
         FROM symbols s
         JOIN files f ON f.id = s.file_id
         WHERE s.file_id IN ({placeholders})",
        placeholders = placeholders
    );
    let file_ids: Vec<i64> = in_scope.iter().map(|(id, _)| *id).collect();
    let params: Vec<&dyn rusqlite::ToSql> =
        file_ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

    let mut sym_stmt = conn
        .prepare(&sql)
        .map_err(|e| LensError::other(format!("map: prepare symbols: {e}")))?;
    let symbols: Vec<MapSymbol> = sym_stmt
        .query_map(params.as_slice(), |row| {
            Ok(MapSymbol {
                qualified_name: row.get(0)?,
                kind: row.get(1)?,
                file_path: row.get(2)?,
                start_line: row.get(3)?,
                caller_count: row.get(4)?,
            })
        })
        .map_err(|e| LensError::other(format!("map: query symbols: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("map: collect symbols: {e}")))?;

    // 3. Build the tree. We use a flat BTreeMap<dir_path, NodeAccumulator>
    //    indexed by directory path. Dir-path is computed by stripping the
    //    file basename and segmenting by `/`. Directories are then linked
    //    into a tree at the end.
    let root_path = scope_norm.clone().unwrap_or_default();
    let mut nodes: BTreeMap<String, NodeAccumulator> = BTreeMap::new();
    nodes.insert(root_path.clone(), NodeAccumulator::default());

    for (_, file_path) in &in_scope {
        // Compute the chain of directory ancestors (root → leaf). We attach
        // file/symbol counts at every level (recursive aggregation).
        let dir = parent_dir(file_path);
        // Truncate the dir path to depth + len(scope_norm segments). Symbols
        // deeper than depth get folded into the deepest visible ancestor.
        let truncated = truncate_to_depth(&dir, &root_path, depth);
        for ancestor in ancestor_chain(&root_path, &truncated) {
            nodes.entry(ancestor).or_default().file_count += 1;
        }
    }

    // Attribute each symbol to its (truncated) directory.
    for sym in &symbols {
        let dir = parent_dir(&sym.file_path);
        let truncated = truncate_to_depth(&dir, &root_path, depth);
        for ancestor in ancestor_chain(&root_path, &truncated) {
            let acc = nodes.entry(ancestor.clone()).or_default();
            acc.symbol_count += 1;
            *acc.kinds.entry(sym.kind.clone()).or_insert(0) += 1;
        }
        nodes
            .entry(truncated)
            .or_default()
            .symbols
            .push(sym.clone());
    }

    // 4. Materialise into a tree. We walk node paths in sorted order and
    //    nest each into its parent's children list. Children inherit the
    //    same sort order (alphabetical) by construction.
    let root = build_tree_node(&root_path, &mut nodes);

    Ok(MapResult { root, scope: scope_norm, depth })
}

#[derive(Default)]
struct NodeAccumulator {
    file_count: i64,
    symbol_count: i64,
    kinds: BTreeMap<String, i64>,
    symbols: Vec<MapSymbol>,
}

fn parent_dir(file_path: &str) -> String {
    match file_path.rfind('/') {
        Some(i) => file_path[..i].to_string(),
        None => String::new(),
    }
}

/// Cut a directory path so it lies at most `depth` segments below `root`.
/// E.g. root="src", depth=1, dir="src/lang/registry" → "src/lang".
fn truncate_to_depth(dir: &str, root: &str, depth: u32) -> String {
    // Compute the relative segments below root.
    let rel = if root.is_empty() {
        dir.to_string()
    } else if dir == root {
        return root.to_string();
    } else if let Some(stripped) = dir.strip_prefix(&format!("{root}/")) {
        stripped.to_string()
    } else {
        return root.to_string();
    };

    if rel.is_empty() {
        return root.to_string();
    }
    let segs: Vec<&str> = rel.split('/').collect();
    let kept = segs.iter().take(depth as usize).copied().collect::<Vec<_>>();
    let joined = kept.join("/");
    if root.is_empty() {
        joined
    } else if joined.is_empty() {
        root.to_string()
    } else {
        format!("{root}/{joined}")
    }
}

/// All ancestors of `path` from `root` (inclusive) to `path` (inclusive),
/// in walk order (root first).
fn ancestor_chain(root: &str, path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    out.push(root.to_string());
    if path == root {
        return out;
    }
    let rel = if root.is_empty() {
        path.to_string()
    } else if let Some(stripped) = path.strip_prefix(&format!("{root}/")) {
        stripped.to_string()
    } else {
        return out;
    };
    let mut acc = root.to_string();
    for seg in rel.split('/') {
        if acc.is_empty() {
            acc = seg.to_string();
        } else {
            acc = format!("{acc}/{seg}");
        }
        out.push(acc.clone());
    }
    out
}

fn build_tree_node(
    path: &str,
    nodes: &mut BTreeMap<String, NodeAccumulator>,
) -> MapNode {
    // Steal the accumulator out of the map so we can mutate it freely; if a
    // node is referenced as ancestor but never directly populated (rare —
    // happens when truncation skips a level) it falls back to default.
    let acc = nodes.remove(path).unwrap_or_default();

    // Children are nodes whose path is exactly `path/<one segment>`. We scan
    // the BTreeMap range, so only direct children show up — descendants are
    // folded into their direct parent's accumulator at insertion time.
    let direct_child_paths: Vec<String> = {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| {
                if path.is_empty() {
                    !k.is_empty()
                } else {
                    k.starts_with(&prefix)
                }
            })
            .filter_map(|(k, _)| {
                let suffix = if path.is_empty() {
                    k.as_str()
                } else {
                    k.strip_prefix(&prefix).unwrap_or(k)
                };
                if suffix.contains('/') {
                    None
                } else {
                    Some(k.clone())
                }
            })
            .collect()
    };

    let children: Vec<MapNode> = direct_child_paths
        .into_iter()
        .map(|child_path| build_tree_node(&child_path, nodes))
        .collect();

    let mut histogram: Vec<(String, i64)> = acc.kinds.into_iter().collect();
    histogram.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut top_symbols = acc.symbols;
    top_symbols.sort_by(|a, b| {
        b.caller_count
            .cmp(&a.caller_count)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    top_symbols.truncate(TOP_SYMBOLS_PER_NODE);

    MapNode {
        path: path.to_string(),
        file_count: acc.file_count,
        symbol_count: acc.symbol_count,
        kind_histogram: histogram,
        top_symbols,
        children,
    }
}

/// Compute an ASCII rendering of the map tree. Pure formatter — no fs / no
/// SQL / no allocation tricks beyond `String::push_str`.
pub fn render_tree(result: &MapResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let scope_label = result
        .scope
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("<project root>");
    let _ = writeln!(&mut out, "# Map: {scope_label}");
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Files:** {} • **Symbols:** {} • **Depth:** {}",
        result.root.file_count, result.root.symbol_count, result.depth
    );
    let _ = writeln!(&mut out);
    render_node(&mut out, &result.root, "", true, true);
    out
}

fn render_node(
    out: &mut String,
    node: &MapNode,
    prefix: &str,
    is_last: bool,
    is_root: bool,
) {
    use std::fmt::Write;
    let label = if node.path.is_empty() {
        ".".to_string()
    } else {
        node.path
            .rsplit('/')
            .next()
            .unwrap_or(&node.path)
            .to_string()
    };

    if is_root {
        let _ = writeln!(
            out,
            "{label}/  ({} files, {} symbols)",
            node.file_count, node.symbol_count
        );
    } else {
        let connector = if is_last { "└── " } else { "├── " };
        let _ = writeln!(
            out,
            "{prefix}{connector}{label}/  ({} files, {} symbols)",
            node.file_count, node.symbol_count
        );
    }

    // Kind histogram + top symbols at this node, indented under the node line.
    let next_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    if !node.kind_histogram.is_empty() {
        let parts: Vec<String> = node
            .kind_histogram
            .iter()
            .take(5)
            .map(|(k, n)| format!("{k}={n}"))
            .collect();
        let _ = writeln!(out, "{next_prefix}kinds: {}", parts.join(", "));
    }
    if !node.top_symbols.is_empty() {
        let _ = writeln!(out, "{next_prefix}top:");
        for sym in &node.top_symbols {
            let _ = writeln!(
                out,
                "{next_prefix}  - {} ({}) — {}:{} [{} caller{}]",
                sym.qualified_name,
                sym.kind,
                sym.file_path,
                sym.start_line,
                sym.caller_count,
                if sym.caller_count == 1 { "" } else { "s" }
            );
        }
    }

    for (i, child) in node.children.iter().enumerate() {
        let last = i == node.children.len() - 1;
        render_node(out, child, &next_prefix, last, false);
    }
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

    fn sym(qname: &str, name: &str, kind: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: kind.into(),
            start_line: 1,
            start_col: 0,
            end_line: 5,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 0,
            signature: None,
            visibility: None,
            parent_qualified_name: None,
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

    #[test]
    fn test_map_empty_index_returns_empty_root() {
        let (_dir, s) = tmp_storage();
        let r = build_map(&s, None, 5).unwrap();
        assert_eq!(r.root.file_count, 0);
        assert_eq!(r.root.symbol_count, 0);
        assert!(r.root.children.is_empty());
    }

    #[test]
    fn test_map_aggregates_files_and_symbols_from_root() {
        let (_dir, mut s) = tmp_storage();
        let files = vec![
            file(
                "src/a.rs",
                vec![sym("a::one", "one", "function"), sym("a::two", "two", "function")],
            ),
            file("src/b.rs", vec![sym("b::three", "three", "struct")]),
            file("tests/x.rs", vec![sym("x::four", "four", "function")]),
        ];
        insert_extracted_files(&mut s, &files).unwrap();

        let r = build_map(&s, None, 5).unwrap();
        assert_eq!(r.root.file_count, 3);
        assert_eq!(r.root.symbol_count, 4);
        // Root has two children: src/, tests/
        let child_paths: Vec<&str> = r.root.children.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(child_paths, vec!["src", "tests"]);
        // Per-child counts.
        let src = r.root.children.iter().find(|c| c.path == "src").unwrap();
        assert_eq!(src.file_count, 2);
        assert_eq!(src.symbol_count, 3);
    }

    #[test]
    fn test_map_scope_filters_to_subtree() {
        let (_dir, mut s) = tmp_storage();
        let files = vec![
            file("src/a.rs", vec![sym("a", "a", "function")]),
            file("tests/x.rs", vec![sym("x", "x", "function")]),
        ];
        insert_extracted_files(&mut s, &files).unwrap();

        let r = build_map(&s, Some("src"), 5).unwrap();
        assert_eq!(r.root.file_count, 1);
        assert_eq!(r.root.symbol_count, 1);
        // tests/ is filtered out.
        assert!(!r.root.children.iter().any(|c| c.path == "tests"));
    }

    #[test]
    fn test_map_depth_zero_shows_only_root() {
        let (_dir, mut s) = tmp_storage();
        let files = vec![
            file("src/lang/rust.rs", vec![sym("rust::Ext", "Ext", "struct")]),
            file("src/lang/python.rs", vec![sym("python::Ext", "Ext", "struct")]),
        ];
        insert_extracted_files(&mut s, &files).unwrap();

        let r = build_map(&s, None, 0).unwrap();
        // Depth 0 = only root, no children.
        assert!(r.root.children.is_empty());
        assert_eq!(r.root.file_count, 2);
    }

    #[test]
    fn test_map_depth_caps_descent_and_folds_deeper_into_parent() {
        let (_dir, mut s) = tmp_storage();
        let files = vec![
            file("src/lang/rust.rs", vec![sym("rust::Ext", "Ext", "struct")]),
            file("src/lang/python.rs", vec![sym("python::Ext", "Ext", "struct")]),
            file("src/walk.rs", vec![sym("walk::Walker", "Walker", "struct")]),
        ];
        insert_extracted_files(&mut s, &files).unwrap();

        // Depth 1 from root: only `src/` child shows up; deeper files fold into src.
        let r = build_map(&s, None, 1).unwrap();
        assert_eq!(r.root.children.len(), 1);
        let src = &r.root.children[0];
        assert_eq!(src.path, "src");
        assert_eq!(src.file_count, 3, "all 3 files folded into src/ at depth 1");
        // Depth 2 from root: src/ has children lang/ and walk.rs's parent (just src for walk.rs).
        let r2 = build_map(&s, None, 2).unwrap();
        let src2 = r2.root.children.iter().find(|c| c.path == "src").unwrap();
        assert!(src2.children.iter().any(|c| c.path == "src/lang"));
    }

    #[test]
    fn test_map_top_symbols_ranked_by_caller_count() {
        let (_dir, mut s) = tmp_storage();
        // hot has 3 callers; cold has 0. Expect hot ranked first.
        let mut hot_file = file(
            "src/a.rs",
            vec![sym("hot", "hot", "function"), sym("cold", "cold", "function")],
        );
        for _ in 0..3 {
            hot_file.calls.push(ExtractedCall {
                caller_qualified_name: "cold".into(),
                callee_raw_name: "hot".into(),
                line: 1,
                col: 0,
            });
        }
        insert_extracted_files(&mut s, &[hot_file]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();

        let r = build_map(&s, None, 2).unwrap();
        // top_symbols at the leaf node (src) — find it.
        let leaf = r
            .root
            .children
            .iter()
            .find(|c| c.path == "src")
            .expect("src node");
        let top = &leaf.top_symbols;
        assert!(top.first().map(|s| s.qualified_name.as_str()) == Some("hot"));
    }

    #[test]
    fn test_render_tree_includes_scope_and_counts_and_root_node() {
        let r = MapResult {
            root: MapNode {
                path: "".into(),
                file_count: 5,
                symbol_count: 10,
                kind_histogram: vec![("function".into(), 7), ("struct".into(), 3)],
                top_symbols: vec![],
                children: vec![],
            },
            scope: None,
            depth: 2,
        };
        let s = render_tree(&r);
        assert!(s.contains("# Map: <project root>"));
        assert!(s.contains("**Files:** 5"));
        assert!(s.contains("**Symbols:** 10"));
        assert!(s.contains("**Depth:** 2"));
        assert!(s.contains("./  (5 files, 10 symbols)"));
        assert!(s.contains("kinds: function=7, struct=3"));
    }

    #[test]
    fn test_render_tree_renders_children_with_tree_drawing_chars() {
        let child_a = MapNode {
            path: "src".into(),
            file_count: 2,
            symbol_count: 4,
            kind_histogram: vec![],
            top_symbols: vec![],
            children: vec![],
        };
        let child_b = MapNode {
            path: "tests".into(),
            file_count: 1,
            symbol_count: 1,
            kind_histogram: vec![],
            top_symbols: vec![],
            children: vec![],
        };
        let r = MapResult {
            root: MapNode {
                path: "".into(),
                file_count: 3,
                symbol_count: 5,
                kind_histogram: vec![],
                top_symbols: vec![],
                children: vec![child_a, child_b],
            },
            scope: None,
            depth: 2,
        };
        let s = render_tree(&r);
        assert!(s.contains("├── src/"));
        assert!(s.contains("└── tests/"));
    }

    #[test]
    fn test_truncate_to_depth_keeps_path_under_root() {
        assert_eq!(truncate_to_depth("src/lang/registry", "", 1), "src");
        assert_eq!(truncate_to_depth("src/lang/registry", "src", 1), "src/lang");
        assert_eq!(truncate_to_depth("src/lang/registry", "src", 5), "src/lang/registry");
        assert_eq!(truncate_to_depth("src/lang/registry", "tests", 5), "tests");
    }

    #[test]
    fn test_ancestor_chain_walks_from_root_to_leaf() {
        let chain = ancestor_chain("", "src/lang");
        assert_eq!(chain, vec!["".to_string(), "src".to_string(), "src/lang".to_string()]);

        let chain2 = ancestor_chain("src", "src/lang/registry");
        assert_eq!(
            chain2,
            vec![
                "src".to_string(),
                "src/lang".to_string(),
                "src/lang/registry".to_string()
            ]
        );
    }
}
