//! lens-core — symbol-aware code index.

pub mod error;
pub mod explain;
pub mod extract;
pub mod fetch;
pub mod follow;
pub mod freshness;
pub mod lang;
pub mod map;
pub mod meter;
pub mod refs;
pub mod parse;
pub mod path;
pub mod query;
pub mod slice;
pub mod storage;
pub mod walk;
pub mod watch;

pub use error::{LensError, Result};
pub use extract::{
    run as run_pipeline, run_on_discovered as run_pipeline_on_discovered, ExtractContext,
    ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol, ExtractedTypeRel,
};
pub use lang::{LanguageExtractor, LanguageId, PythonExtractor, Registry, RustExtractor};
pub use explain::{explain_symbol, ExplainResult};
pub use fetch::{fetch_to_raw, FetchResult};
pub use follow::{follow_symbol, FollowResult, CHARS_PER_TOKEN, MAX_CALLERS};
pub use freshness::{ensure_fresh, Config as FreshnessConfig, FreshnessOutcome};
pub use map::{build_map, render_tree as render_map, MapNode, MapResult, MapSymbol, TOP_SYMBOLS_PER_NODE};
pub use meter::{
    meter_path, parse_state as parse_meter_state, read_state as read_meter,
    record as record_meter, render_state as render_meter_state, reset as reset_meter,
    snapshot_invocation as snapshot_meter, write_state as write_meter, MeterCounters, MeterState,
};
pub use refs::{list_refs, RefSite, RefsResult, HARD_LIMIT as REFS_HARD_LIMIT};
pub use slice::{slice_at, SliceResult, MAX_IMPORTS};
pub use parse::{parse, ParsedFile};
pub use path::{resolve_symbol_to_id, shortest_path, PathResult};
pub use query::{
    query_graph, seed_nodes_from_question, EdgeKind, Graph, QueryEdge, QueryNode, QueryResult,
    SymbolMeta, TraversalMode,
};
pub use storage::{
    diff_against_index, insert_extracted_files, resolve_cross_file_references, update_files,
    FileDiff, InsertStats, ResolveStats, Storage, UpdateStats,
};
pub use walk::{discover, DiscoveredFile};
pub use watch::{run_watch, WatchConfig};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_version_returns_non_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn test_core_version_matches_semver_shape() {
        let v = version();
        let parts: Vec<&str> = v.split('.').collect();
        assert!(parts.len() >= 2, "expected semver-like version, got {v}");
        for p in &parts[..2] {
            assert!(p.chars().all(|c| c.is_ascii_digit()), "non-numeric segment in version: {v}");
        }
    }

    #[test]
    fn test_core_re_exports_error_types() {
        let _: Result<()> = Ok(());
        let _: LensError = LensError::other("smoke");
    }
}
