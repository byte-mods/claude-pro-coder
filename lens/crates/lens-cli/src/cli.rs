use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "lens",
    version,
    about = "Symbol-aware code map for Claude — replaces graphify with a precomputed Ctrl+Click index.",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to operate on (graphify-compat: `lens <path>` indexes it).
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Incremental rebuild (graphify-compat: `lens . --update`).
    #[arg(long)]
    pub update: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a `.lens/` index in the project.
    Init {
        /// Project root (defaults to current directory).
        path: Option<PathBuf>,
        /// Skip modifying .gitignore.
        #[arg(long)]
        no_gitignore: bool,
    },
    /// Build the full index from scratch.
    Index,
    /// Re-extract changed files (incremental update).
    Update,
    /// BFS/DFS query over the graph (graphify-compat).
    Query {
        /// The question to ask.
        question: String,
        /// Use depth-first traversal instead of breadth-first.
        #[arg(long)]
        dfs: bool,
        /// Token budget cap for the answer.
        #[arg(long, default_value_t = 2000)]
        budget: u32,
    },
    /// Ctrl+Click — definition + minimal slice for a symbol.
    Follow {
        /// Symbol name (qualified or unqualified).
        symbol: String,
        /// Originating file:line for disambiguation.
        #[arg(long, value_name = "FILE:LINE")]
        from: Option<String>,
        /// Token budget cap.
        #[arg(long, default_value_t = 2000)]
        budget: u32,
    },
    /// List callers of a symbol.
    Refs {
        /// Symbol name (qualified or unqualified).
        symbol: String,
        /// Maximum number of callers to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Minimal context for a file:line location.
    Slice {
        /// Location in `path:line` form.
        location: String,
        /// Token budget cap.
        #[arg(long, default_value_t = 2000)]
        budget: u32,
    },
    /// Fetch a URL and save it under `.lens/raw/` (graphify-compat: `add <url>`).
    /// If the file's extension matches a registered language, the file is
    /// also indexed.
    Add {
        /// URL to fetch (http(s) or file://).
        url: String,
    },
    /// Shortest path between two symbols (graphify-compat: `path "A" "B"`).
    Path {
        /// Source symbol (qualified name preferred; bare name allowed).
        from: String,
        /// Destination symbol.
        to: String,
    },
    /// Plain-language explanation of a symbol and its neighbors
    /// (graphify-compat: `explain "X"`).
    Explain {
        /// Symbol to explain (qualified name preferred; bare name allowed).
        symbol: String,
    },
    /// Architecture summary of the project (or a scope).
    Map {
        /// Restrict the summary to a sub-tree.
        #[arg(long)]
        scope: Option<PathBuf>,
        /// Maximum traversal depth.
        #[arg(long, default_value_t = 2)]
        depth: u32,
    },
    /// Token meter (input/output, persistent across /clear).
    Meter {
        /// JSON output.
        #[arg(long)]
        json: bool,
        /// Show delta over the last duration (e.g. 1h, 30m). v1: requires
        /// last_updated to be within the window; otherwise reports zeros.
        #[arg(long)]
        since: Option<String>,
        /// Reset counters.
        #[arg(long)]
        reset: bool,
        /// Show delta since last invocation.
        #[arg(long)]
        diff: bool,
        /// Record input tokens consumed (incremental). Combine with --record-output.
        #[arg(long)]
        record_input: Option<u64>,
        /// Record output tokens produced (incremental). Combine with --record-input.
        #[arg(long)]
        record_output: Option<u64>,
    },
    /// Watch the project and reindex on file changes.
    Watch {
        /// Debounce window in milliseconds.
        #[arg(long, default_value_t = 200)]
        debounce: u64,
    },
    /// Run as a stdio MCP server (for Claude Code).
    Mcp,
}
