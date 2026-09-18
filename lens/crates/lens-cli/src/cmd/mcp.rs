//! `lens mcp` — stdio MCP server.
//!
//! Implements a minimal subset of the Model Context Protocol over
//! newline-delimited JSON-RPC 2.0. Exposes the lens verbs (follow, refs,
//! query, explain, path, slice, map, search, deps) as MCP tools so any MCP
//! client — Claude Code, Codex, Cursor, a custom agent — can call them as
//! structured tools rather than shelling out.
//!
//! Protocol (subset):
//!   - `initialize` — handshake, returns server info + capabilities.
//!   - `initialized` (notification) — client signals readiness; no response.
//!   - `ping` — liveness; returns `{}`.
//!   - `tools/list` — returns the tool catalogue with input JSON schemas.
//!   - `tools/call` — invokes a tool by name with arguments; returns
//!     `{ content: [{ type: "text", text }], isError }`.
//!   - `shutdown` / `exit` — graceful termination.
//!
//! Anything not in the subset above returns a `MethodNotFound` error.
//!
//! ## Robustness guarantees (v0.2)
//!
//! - **Auto-freshness on every call.** The CLI verbs ran the freshness
//!   check; the v1 MCP path did not, so a long-lived server answered from a
//!   stale index after edits. Every `tools/call` now runs the same throttled
//!   `ensure_fresh` as the CLI (stat fast path → cheap).
//! - **Auto-bootstrap.** A missing `.lens/index.db` no longer errors; the
//!   server initialises and fully indexes the root on first use so a model
//!   can start on a fresh checkout without a shell step.
//! - **Graph cache.** `query` / `explain` / `path` need the in-memory graph.
//!   It is cached per root and invalidated by an index stamp (row counts +
//!   last `indexed_at`), so back-to-back calls do not rescan SQLite.
//! - **Multi-project.** Every tool accepts an optional `root` argument;
//!   the server default is `--root` or the spawn directory.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lens_core::{
    build_map, ensure_fresh, explain_symbol, file_deps, follow_symbol, list_refs, query_graph,
    resolve_symbol_candidates, resolve_symbol_to_id, search_docs, shortest_path, slice_at,
    FreshnessConfig, Graph, SearchOptions, Storage, TraversalMode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "lens";
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

/// Entry point — runs the loop until stdin closes or `exit` is received.
/// `0` on clean shutdown, non-zero on stream error.
pub fn run(root: Option<&Path>) -> Result<(), u8> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let cwd = match root {
        Some(r) => r.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("lens mcp: cannot resolve current directory: {e}");
                return Err(1);
            }
        },
    };
    if !cwd.is_dir() {
        eprintln!("lens mcp: root '{}' is not a directory", cwd.display());
        return Err(1);
    }
    serve(BufReader::new(stdin.lock()), stdout.lock(), &cwd)
}

/// Index stamp used to invalidate the cached graph. Cheap to compute (three
/// aggregate lookups) and changes whenever any index write lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndexStamp {
    files: i64,
    symbols: i64,
    calls_resolved: i64,
    last_indexed: i64,
}

impl IndexStamp {
    fn read(storage: &Storage) -> Option<Self> {
        storage
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM files),
                        (SELECT COUNT(*) FROM symbols),
                        (SELECT COUNT(*) FROM calls WHERE callee_symbol_id IS NOT NULL),
                        COALESCE((SELECT MAX(indexed_at) FROM files), 0)",
                [],
                |r| Ok(IndexStamp { files: r.get(0)?, symbols: r.get(1)?, calls_resolved: r.get(2)?, last_indexed: r.get(3)? }),
            )
            .ok()
    }
}

/// Server state shared across requests: the per-root graph cache.
#[derive(Default)]
pub struct Server {
    graphs: Mutex<HashMap<PathBuf, (IndexStamp, Arc<Graph>)>>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load (or reuse) the in-memory graph for `root`.
    fn graph_for(&self, root: &Path, storage: &Storage) -> Result<Arc<Graph>, McpError> {
        let stamp = IndexStamp::read(storage);
        if let Some(stamp) = stamp {
            if let Ok(cache) = self.graphs.lock() {
                if let Some((cached_stamp, graph)) = cache.get(root) {
                    if *cached_stamp == stamp {
                        return Ok(Arc::clone(graph));
                    }
                }
            }
        }
        let graph = Arc::new(Graph::load(storage).map_err(|e| McpError::internal(format!("graph: {e}")))?);
        if let (Some(stamp), Ok(mut cache)) = (stamp, self.graphs.lock()) {
            cache.insert(root.to_path_buf(), (stamp, Arc::clone(&graph)));
        }
        Ok(graph)
    }

    /// Parse a single line and dispatch. Returns `Some(response)` for
    /// requests (JSON-RPC messages with an `id`), `None` for notifications.
    pub fn handle_message(&self, line: &str, default_root: &Path) -> Option<Value> {
        let raw: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_response(Value::Null, PARSE_ERROR, format!("parse error: {e}")));
            }
        };

        let id = raw.get("id").cloned();
        let method = match raw.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => {
                if let Some(id) = id {
                    return Some(error_response(id, INVALID_REQUEST, "missing method".into()));
                }
                return None;
            }
        };
        let params = raw.get("params").cloned().unwrap_or(Value::Null);
        let is_notification = id.is_none();

        let result = self.dispatch(&method, &params, default_root);
        if is_notification {
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        Some(match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err(McpError { code, message }) => error_response(id, code, message),
        })
    }

    fn dispatch(&self, method: &str, params: &Value, default_root: &Path) -> Result<Value, McpError> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
            })),
            "initialized" | "notifications/initialized" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "shutdown" => Ok(Value::Null),
            "tools/list" => Ok(json!({ "tools": tool_catalogue() })),
            "tools/call" => self.handle_tool_call(params, default_root),
            other => Err(McpError::method_not_found(format!("method not found: {other}"))),
        }
    }

    fn handle_tool_call(&self, params: &Value, default_root: &Path) -> Result<Value, McpError> {
        #[derive(Deserialize)]
        struct CallParams {
            name: String,
            #[serde(default)]
            arguments: Value,
        }
        let cp: CallParams = serde_json::from_value(params.clone())
            .map_err(|e| McpError::invalid_params(format!("tools/call params: {e}")))?;

        let root = resolve_root(&cp.arguments, default_root)?;
        let mut storage = open_or_bootstrap(&root)?;
        // Same throttled freshness check as the CLI — errors never block a read.
        let _ = ensure_fresh(&mut storage, &root, FreshnessConfig::from_env());

        let text = match cp.name.as_str() {
            "lens_follow" => tool_follow(&cp.arguments, &storage, &root)?,
            "lens_refs" => tool_refs(&cp.arguments, &storage, &root)?,
            "lens_query" => tool_query(&cp.arguments, &storage, &root)?,
            "lens_explain" => tool_explain(&cp.arguments, &storage, &root, self)?,
            "lens_path" => tool_path(&cp.arguments, &storage, &root, self)?,
            "lens_slice" => tool_slice(&cp.arguments, &storage, &root)?,
            "lens_map" => tool_map(&cp.arguments, &storage, &root)?,
            "lens_search" => tool_search(&cp.arguments, &storage, &root)?,
            "lens_deps" => tool_deps(&cp.arguments, &storage, &root)?,
            other => {
                return Err(McpError::invalid_params(format!("unknown tool: {other}")));
            }
        };

        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false
        }))
    }
}

/// Pure I/O loop — separated from `run()` for testability. Reads lines from
/// `reader`, dispatches each as a JSON-RPC request, writes the response (if
/// any) to `writer`. Returns when EOF is reached or `exit` is processed.
pub fn serve<R: BufRead, W: Write>(mut reader: R, mut writer: W, project_root: &Path) -> Result<(), u8> {
    let server = Server::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("lens mcp: read error: {e}");
                return Err(1);
            }
        };
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match server.handle_message(trimmed, project_root) {
            Some(r) => r,
            None => continue,
        };
        let serialised = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("lens mcp: serialise: {e}");
                continue;
            }
        };
        if let Err(e) = writeln!(writer, "{serialised}") {
            eprintln!("lens mcp: write error: {e}");
            return Err(1);
        }
        if let Err(e) = writer.flush() {
            eprintln!("lens mcp: flush error: {e}");
            return Err(1);
        }
    }
}

/// Stateless convenience for tests and one-shot callers: a fresh
/// [`Server`] per message (no graph cache carried across calls).
#[cfg_attr(not(test), allow(dead_code))]
pub fn handle_message(line: &str, project_root: &Path) -> Option<Value> {
    Server::new().handle_message(line, project_root)
}

#[derive(Debug)]
struct McpError {
    code: i32,
    message: String,
}

impl McpError {
    fn invalid_params(s: impl Into<String>) -> Self {
        Self { code: INVALID_PARAMS, message: s.into() }
    }
    fn internal(s: impl Into<String>) -> Self {
        Self { code: INTERNAL_ERROR, message: s.into() }
    }
    fn method_not_found(s: impl Into<String>) -> Self {
        Self { code: METHOD_NOT_FOUND, message: s.into() }
    }
}

fn error_response(id: Value, code: i32, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

/// Shared `root` property appended to every tool schema.
fn root_property() -> Value {
    json!({ "type": "string", "description": "Project root to operate on (absolute path). Defaults to the server's root. Lets one server serve several projects." })
}

/// The catalogue of tools advertised to the client. Keep schemas minimal —
/// each tool's primary input is a single string plus a few numeric knobs.
pub fn tool_catalogue() -> Vec<Value> {
    vec![
        json!({
            "name": "lens_follow",
            "description": "Ctrl+Click — definition + doc comment + signature + budget-fitted body slice + nearest callers for a symbol. Token-efficient replacement for reading whole files. Output ends with a tokens-emitted / tokens-saved footer.",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name (qualified or unqualified)." },
                    "from":   { "type": "string", "description": "Disambiguation hint of the form FILE:LINE." },
                    "budget": { "type": "integer", "minimum": 0, "description": "Token budget for the body slice. Default 1500." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_refs",
            "description": "List callers and reference sites of a symbol with file:line anchors. Use for impact analysis before changing a signature.",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": {
                    "symbol": { "type": "string" },
                    "limit":  { "type": "integer", "minimum": 1, "description": "Maximum reference sites to return. Default 20." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_query",
            "description": "BFS/DFS query over the symbol graph for a topic phrased in words. Returns budget-capped seed symbols and their neighbourhood.",
            "inputSchema": {
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": { "type": "string" },
                    "dfs":      { "type": "boolean", "description": "Depth-first traversal instead of breadth-first. Default false." },
                    "budget":   { "type": "integer", "minimum": 0, "description": "Token budget. Default 2000." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_explain",
            "description": "Plain-language summary of a symbol and its neighbors (parents, children, callers, callees, types, imports).",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": { "symbol": { "type": "string" }, "root": root_property() }
            }
        }),
        json!({
            "name": "lens_path",
            "description": "Shortest path between two symbols in the call/reference graph.",
            "inputSchema": {
                "type": "object",
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "to":   { "type": "string" },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_slice",
            "description": "Minimal context slice for a file:line location (enclosing symbol, signature, body, same-file imports), fitted to a token budget.",
            "inputSchema": {
                "type": "object",
                "required": ["location"],
                "properties": {
                    "location": { "type": "string", "description": "FILE:LINE form, project-relative." },
                    "budget":   { "type": "integer", "minimum": 0, "description": "Token budget. Default 1500." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_map",
            "description": "Architecture summary of the project (or a sub-tree): files/symbols per directory with hot-spot symbols. Depth is reduced automatically to fit the budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "Restrict to a sub-tree (project-relative path)." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Maximum traversal depth. Default 2." },
                    "budget": { "type": "integer", "minimum": 0, "description": "Token budget. Default 2000." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_search",
            "description": "Keyword search over every text file in the project — any language (including ones without a symbol extractor), docs, config, and the agent's own notes under .claude/state. Returns budget-capped file:line hits with the enclosing symbol. Use instead of grep.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query":  { "type": "string", "description": "Words to find; each is prefix-matched. OR / AND / NOT supported." },
                    "budget": { "type": "integer", "minimum": 0, "description": "Token budget. Default 2000." },
                    "limit":  { "type": "integer", "minimum": 1, "description": "Maximum files to report. Default 20." },
                    "scope":  { "type": "string", "description": "Restrict to a sub-tree (project-relative path)." },
                    "kind":   { "type": "string", "enum": ["code", "text"], "description": "Restrict to code files or text/docs." },
                    "root": root_property()
                }
            }
        }),
        json!({
            "name": "lens_deps",
            "description": "File-level connections: what a file imports, who imports it, which files it calls into and which call into it, and its most-called symbols. Use to size the blast radius of a change without reading the file.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "description": "Project-relative file path." },
                    "root": root_property()
                }
            }
        }),
    ]
}

fn resolve_root(args: &Value, default_root: &Path) -> Result<PathBuf, McpError> {
    match args.get("root").and_then(|v| v.as_str()) {
        Some(r) if !r.trim().is_empty() => {
            let p = PathBuf::from(r);
            if !p.is_dir() {
                return Err(McpError::invalid_params(format!("root '{r}' is not a directory")));
            }
            Ok(p)
        }
        _ => Ok(default_root.to_path_buf()),
    }
}

/// Open `.lens/index.db` under `root`, building it first if it is missing.
fn open_or_bootstrap(root: &Path) -> Result<Storage, McpError> {
    let db_path: PathBuf = root.join(".lens").join("index.db");
    if !db_path.exists() {
        eprintln!("lens mcp: no index at {} — building one now", db_path.display());
        crate::cmd::index::run(Some(root))
            .map_err(|code| McpError::internal(format!("auto-index of {} failed (exit {code})", root.display())))?;
    }
    Storage::open(&db_path).map_err(|e| McpError::internal(format!("open storage: {e}")))
}

// ----- Per-tool handlers -----

#[derive(Deserialize)]
struct FollowArgs {
    symbol: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default = "default_budget_1500")]
    budget: u32,
}
fn tool_follow(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: FollowArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_follow: {e}")))?;
    let mut ids = resolve_symbol_candidates(storage, &a.symbol)
        .map_err(|e| McpError::internal(format!("resolve: {e}")))?;
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!("no symbol matched '{}'", a.symbol)));
    }
    if let Some(from_str) = &a.from {
        if let Some((file, _line)) = parse_file_line(from_str) {
            ids.retain(|c| c.file_path == file);
            if ids.is_empty() {
                return Err(McpError::invalid_params(format!("--from '{from_str}' did not match any candidate")));
            }
        }
    }
    if ids.len() > 1 {
        let mut msg = format!("'{}' is ambiguous ({} candidates) — pass a qualified name or from=FILE:LINE:\n", a.symbol, ids.len());
        for m in ids.iter().take(10) {
            msg.push_str(&format!("  - [{}] {} ({} at {}:{})\n", m.language, m.qualified_name, m.kind, m.file_path, m.start_line));
        }
        return Err(McpError::invalid_params(msg));
    }
    let result = follow_symbol(storage, root, ids[0].symbol_id, a.budget)
        .map_err(|e| McpError::internal(format!("follow: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("follow returned None for resolved id")))?;
    let focus_file = result.focus.file_path.clone();
    let rendered = crate::cmd::follow::render_markdown(&a.symbol, &result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &[focus_file.as_str()]))
}

#[derive(Deserialize)]
struct RefsArgs {
    symbol: String,
    #[serde(default = "default_limit_20")]
    limit: u32,
}
fn tool_refs(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: RefsArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_refs: {e}")))?;
    let ids = resolve_symbol_candidates(storage, &a.symbol)
        .map_err(|e| McpError::internal(format!("resolve: {e}")))?;
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!("no symbol matched '{}'", a.symbol)));
    }
    if ids.len() > 1 {
        let mut msg = format!("'{}' is ambiguous ({} candidates) — use a qualified name:\n", a.symbol, ids.len());
        for m in ids.iter().take(10) {
            msg.push_str(&format!("  - [{}] {} ({} at {}:{})\n", m.language, m.qualified_name, m.kind, m.file_path, m.start_line));
        }
        return Err(McpError::invalid_params(msg));
    }
    let result = list_refs(storage, ids[0].symbol_id, a.limit)
        .map_err(|e| McpError::internal(format!("refs: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("refs returned None for resolved id")))?;
    let mut touched: Vec<&str> = vec![result.focus.file_path.as_str()];
    touched.extend(result.sites.iter().map(|s| s.file_path.as_str()));
    let rendered = crate::cmd::refs::render_markdown(&a.symbol, a.limit, &result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &touched))
}

#[derive(Deserialize)]
struct QueryArgs {
    question: String,
    #[serde(default)]
    dfs: bool,
    #[serde(default = "default_budget_2000")]
    budget: u32,
}
fn tool_query(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: QueryArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_query: {e}")))?;
    let mode = if a.dfs { TraversalMode::Dfs } else { TraversalMode::Bfs };
    let result = query_graph(storage, &a.question, mode, a.budget)
        .map_err(|e| McpError::internal(format!("query: {e}")))?;
    let touched: Vec<&str> = result.nodes.iter().map(|n| n.file_path.as_str()).collect();
    let rendered = crate::cmd::query::render_markdown(&a.question, &result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &touched))
}

#[derive(Deserialize)]
struct ExplainArgs {
    symbol: String,
}
fn tool_explain(args: &Value, storage: &Storage, root: &Path, server: &Server) -> Result<String, McpError> {
    let a: ExplainArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_explain: {e}")))?;
    let graph = server.graph_for(root, storage)?;
    let ids = resolve_symbol_to_id(&graph, &a.symbol);
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!("no symbol matched '{}'", a.symbol)));
    }
    if ids.len() > 1 {
        return Err(McpError::invalid_params(format!("'{}' is ambiguous ({} candidates)", a.symbol, ids.len())));
    }
    let result = explain_symbol(&graph, ids[0])
        .map_err(|e| McpError::internal(format!("explain: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("explain returned None for resolved id")))?;
    let mut touched: Vec<&str> = vec![result.focus.file_path.as_str()];
    for bucket in [&result.parents, &result.children, &result.callers, &result.callees, &result.types, &result.imports] {
        touched.extend(bucket.iter().map(|n| n.file_path.as_str()));
    }
    let rendered = crate::cmd::explain::render_markdown(&result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &touched))
}

#[derive(Deserialize)]
struct PathArgs {
    from: String,
    to: String,
}
fn tool_path(args: &Value, storage: &Storage, root: &Path, server: &Server) -> Result<String, McpError> {
    let a: PathArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_path: {e}")))?;
    let graph = server.graph_for(root, storage)?;
    let from_ids = resolve_symbol_to_id(&graph, &a.from);
    let to_ids = resolve_symbol_to_id(&graph, &a.to);
    if from_ids.len() != 1 {
        return Err(McpError::invalid_params(format!("'from' must resolve to exactly one symbol; got {}", from_ids.len())));
    }
    if to_ids.len() != 1 {
        return Err(McpError::invalid_params(format!("'to' must resolve to exactly one symbol; got {}", to_ids.len())));
    }
    let result = shortest_path(&graph, from_ids[0], to_ids[0])
        .map_err(|e| McpError::internal(format!("path: {e}")))?;
    let touched: Vec<&str> = result
        .as_ref()
        .map(|p| p.nodes.iter().map(|n| n.file_path.as_str()).collect())
        .unwrap_or_default();
    let rendered = crate::cmd::path::render_markdown(&a.from, &a.to, result.as_ref());
    Ok(crate::cmd::util::finalize(root, storage, rendered, &touched))
}

#[derive(Deserialize)]
struct SliceArgs {
    location: String,
    #[serde(default = "default_budget_1500")]
    budget: u32,
}
fn tool_slice(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: SliceArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_slice: {e}")))?;
    let (file, line) = parse_file_line(&a.location)
        .ok_or_else(|| McpError::invalid_params(format!("location must be FILE:LINE; got '{}'", a.location)))?;
    let result = slice_at(storage, root, file, line, a.budget)
        .map_err(|e| McpError::internal(format!("slice: {e}")))?
        .ok_or_else(|| McpError::invalid_params(format!("no symbol covers {file}:{line}")))?;
    let focus_file = result.focus.file_path.clone();
    let rendered = crate::cmd::slice::render_markdown(&a.location, &result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &[focus_file.as_str()]))
}

#[derive(Deserialize)]
struct MapArgs {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default = "default_depth_2")]
    depth: u32,
    #[serde(default = "default_budget_2000")]
    budget: u32,
}
fn tool_map(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: MapArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_map: {e}")))?;
    // Validate the scope resolves (build_map tolerates unknown scopes by
    // returning an empty tree — same as the CLI).
    let _ = build_map(storage, a.scope.as_deref(), 0).map_err(|e| McpError::internal(format!("map: {e}")))?;
    let rendered = crate::cmd::map::render_within_budget(storage, a.scope.as_deref(), a.depth, a.budget)
        .map_err(|e| McpError::internal(format!("map: {e}")))?;
    Ok(crate::cmd::util::finalize(root, storage, rendered, &[]))
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_budget_2000")]
    budget: u32,
    #[serde(default = "default_limit_20")]
    limit: u32,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}
fn tool_search(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: SearchArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_search: {e}")))?;
    if a.query.trim().is_empty() {
        return Err(McpError::invalid_params("lens_search: query must not be empty"));
    }
    if let Some(k) = &a.kind {
        if k != "code" && k != "text" {
            return Err(McpError::invalid_params(format!("lens_search: kind must be code or text (got '{k}')")));
        }
    }
    let opts = SearchOptions { limit: a.limit, budget: a.budget, scope: a.scope.clone(), kind: a.kind.clone() };
    let result = search_docs(storage, &a.query, &opts).map_err(|e| McpError::internal(format!("search: {e}")))?;
    let touched: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    let rendered = crate::cmd::search::render_markdown(&result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &touched))
}

#[derive(Deserialize)]
struct DepsArgs {
    path: String,
}
fn tool_deps(args: &Value, storage: &Storage, root: &Path) -> Result<String, McpError> {
    let a: DepsArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_deps: {e}")))?;
    let rel = lens_core::docs::scope_to_relative(root, Path::new(&a.path))
        .ok_or_else(|| McpError::invalid_params(format!("path '{}' is not under the project root", a.path)))?;
    let result = file_deps(storage, &rel)
        .map_err(|e| McpError::internal(format!("deps: {e}")))?
        .ok_or_else(|| McpError::invalid_params(format!("'{rel}' is not in the symbol index; try lens_search for text-only files")))?;
    let rendered = crate::cmd::deps::render_markdown(&result);
    Ok(crate::cmd::util::finalize(root, storage, rendered, &[rel.as_str()]))
}

fn parse_file_line(s: &str) -> Option<(&str, u32)> {
    let (file, line) = s.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((file, line))
}

fn default_budget_1500() -> u32 {
    1500
}
fn default_budget_2000() -> u32 {
    2000
}
fn default_limit_20() -> u32 {
    20
}
fn default_depth_2() -> u32 {
    2
}

#[derive(Serialize, Deserialize, Debug)]
#[allow(dead_code)]
struct Request<'a> {
    jsonrpc: &'a str,
    id: Option<u64>,
    method: &'a str,
    params: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    fn project_with_index() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn target() {}\npub fn caller() { target(); }\n");
        write(&root.join("NOTES.md"), "# Notes\n\nremember: target is the hot path\n");
        crate::cmd::index::run(Some(root)).expect("initial index");
        dir
    }

    fn assert_jsonrpc_response(v: &Value, expect_error: bool) {
        assert_eq!(v.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
        assert!(v.get("id").is_some());
        if expect_error {
            assert!(v.get("error").is_some(), "expected error: {v}");
        } else {
            assert!(v.get("result").is_some(), "expected result: {v}");
        }
    }

    fn call(dir: &Path, id: u64, name: &str, args: Value) -> Value {
        let req = json!({
            "jsonrpc": "2.0", "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        })
        .to_string();
        handle_message(&req, dir).expect("response expected")
    }

    fn text_of(resp: &Value) -> String {
        resp.pointer("/result/content/0/text").and_then(|v| v.as_str()).unwrap_or_default().to_string()
    }

    #[test]
    fn test_mcp_initialize_returns_protocol_version_and_server_info() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string();
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        assert_jsonrpc_response(&resp, false);
        let result = resp.get("result").unwrap();
        assert_eq!(result.get("protocolVersion").and_then(|v| v.as_str()), Some(PROTOCOL_VERSION));
        assert_eq!(
            result.get("serverInfo").and_then(|v| v.get("name")).and_then(|v| v.as_str()),
            Some(SERVER_NAME)
        );
    }

    #[test]
    fn test_mcp_initialized_notification_produces_no_response() {
        let req = json!({"jsonrpc":"2.0","method":"initialized","params":{}}).to_string();
        let dir = tempfile::tempdir().unwrap();
        assert!(handle_message(&req, dir.path()).is_none());
    }

    #[test]
    fn test_mcp_ping_returns_empty_object() {
        let req = json!({"jsonrpc":"2.0","id":9,"method":"ping"}).to_string();
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        assert_eq!(resp.get("result"), Some(&json!({})));
    }

    #[test]
    fn test_mcp_tools_list_returns_all_nine_tools_with_root_property() {
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string();
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        let tools = resp.pointer("/result/tools").unwrap().as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.get("name").and_then(|n| n.as_str()).unwrap()).collect();
        for expected in [
            "lens_follow", "lens_refs", "lens_query", "lens_explain", "lens_path",
            "lens_slice", "lens_map", "lens_search", "lens_deps",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        assert_eq!(tools.len(), 9);
        for t in tools {
            assert!(t.pointer("/inputSchema/properties/root").is_some(), "tool {} lacks root", t["name"]);
        }
    }

    #[test]
    fn test_mcp_tools_call_follow_returns_text_block_with_token_footer() {
        let dir = project_with_index();
        let resp = call(dir.path(), 3, "lens_follow", json!({ "symbol": "target", "budget": 1000 }));
        assert_jsonrpc_response(&resp, false);
        let content = resp.pointer("/result/content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].get("type").and_then(|v| v.as_str()), Some("text"));
        let text = text_of(&resp);
        assert!(text.contains("# Follow:") && text.contains("target"), "{text}");
        assert!(text.contains("_tokens: ~"), "footer missing: {text}");
    }

    #[test]
    fn test_mcp_tools_call_refs_returns_text_block() {
        let dir = project_with_index();
        let resp = call(dir.path(), 4, "lens_refs", json!({ "symbol": "target", "limit": 10 }));
        let text = text_of(&resp);
        assert!(text.contains("# Refs:") && text.contains("target"), "{text}");
    }

    #[test]
    fn test_mcp_tools_call_map_returns_tree() {
        let dir = project_with_index();
        let resp = call(dir.path(), 5, "lens_map", json!({ "depth": 2 }));
        assert!(text_of(&resp).contains("# Map:"));
    }

    #[test]
    fn test_mcp_tools_call_search_finds_markdown_note() {
        let dir = project_with_index();
        let resp = call(dir.path(), 11, "lens_search", json!({ "query": "hot path" }));
        assert_jsonrpc_response(&resp, false);
        let text = text_of(&resp);
        assert!(text.contains("NOTES.md"), "{text}");
        let bad = call(dir.path(), 12, "lens_search", json!({ "query": "x", "kind": "blob" }));
        assert_jsonrpc_response(&bad, true);
    }

    #[test]
    fn test_mcp_tools_call_deps_returns_card_and_rejects_unknown_file() {
        let dir = project_with_index();
        let resp = call(dir.path(), 13, "lens_deps", json!({ "path": "a.rs" }));
        assert_jsonrpc_response(&resp, false);
        assert!(text_of(&resp).contains("# Deps: `a.rs`"));
        let bad = call(dir.path(), 14, "lens_deps", json!({ "path": "nope.rs" }));
        assert_eq!(bad.pointer("/error/code").and_then(|v| v.as_i64()), Some(INVALID_PARAMS as i64));
    }

    #[test]
    fn test_mcp_tools_call_explain_and_path_share_cached_graph() {
        let dir = project_with_index();
        let server = Server::new();
        let req = |id: u64, name: &str, args: Value| {
            json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": args } }).to_string()
        };
        let r1 = server.handle_message(&req(1, "lens_explain", json!({ "symbol": "target" })), dir.path()).unwrap();
        assert_jsonrpc_response(&r1, false);
        assert_eq!(server.graphs.lock().unwrap().len(), 1, "graph cached after first load");
        let r2 = server.handle_message(&req(2, "lens_path", json!({ "from": "caller", "to": "target" })), dir.path()).unwrap();
        assert_jsonrpc_response(&r2, false);
        assert!(text_of(&r2).contains("# Path:"));
        assert_eq!(server.graphs.lock().unwrap().len(), 1, "same root reuses the cache entry");
    }

    #[test]
    fn test_mcp_root_argument_selects_another_project() {
        let a = project_with_index();
        let b = tempfile::tempdir().unwrap();
        write(&b.path().join("other.py"), "def unique_in_b():\n    pass\n");
        crate::cmd::index::run(Some(b.path())).unwrap();
        // Default root is A; asking for B's symbol with root=B must work.
        let resp = call(a.path(), 20, "lens_follow", json!({ "symbol": "unique_in_b", "root": b.path().to_string_lossy() }));
        assert_jsonrpc_response(&resp, false);
        let bad = call(a.path(), 21, "lens_follow", json!({ "symbol": "unique_in_b" }));
        assert_jsonrpc_response(&bad, true);
        let nodir = call(a.path(), 22, "lens_follow", json!({ "symbol": "x", "root": "/definitely/not/a/dir" }));
        assert_eq!(nodir.pointer("/error/code").and_then(|v| v.as_i64()), Some(INVALID_PARAMS as i64));
    }

    #[test]
    fn test_mcp_tools_call_unknown_tool_returns_invalid_params_error() {
        let dir = project_with_index();
        let resp = call(dir.path(), 6, "lens_nonexistent", json!({}));
        assert_jsonrpc_response(&resp, true);
        assert_eq!(resp.pointer("/error/code").and_then(|v| v.as_i64()), Some(INVALID_PARAMS as i64));
    }

    #[test]
    fn test_mcp_unknown_method_returns_method_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({"jsonrpc":"2.0","id":7,"method":"weather/today"}).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        assert_eq!(resp.pointer("/error/code").and_then(|v| v.as_i64()), Some(METHOD_NOT_FOUND as i64));
    }

    #[test]
    fn test_mcp_malformed_json_returns_parse_error_with_null_id() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_message("{not json", dir.path()).expect("response expected");
        assert_eq!(resp.pointer("/error/code").and_then(|v| v.as_i64()), Some(PARSE_ERROR as i64));
        assert!(resp.get("id").is_some_and(|v| v.is_null()));
    }

    #[test]
    fn test_mcp_serve_loop_processes_multiple_messages_until_eof() {
        let dir = project_with_index();
        let input = format!(
            "{}\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        );
        let mut output: Vec<u8> = Vec::new();
        let r = serve(input.as_bytes(), &mut output, dir.path());
        assert_eq!(r, Ok(()));
        let s = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 responses, got {}", lines.len());
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            assert!(v.get("result").is_some());
        }
    }

    #[test]
    fn test_mcp_serve_loop_skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut output: Vec<u8> = Vec::new();
        let r = serve("\n\n\n".as_bytes(), &mut output, dir.path());
        assert_eq!(r, Ok(()));
        assert!(output.is_empty());
    }

    #[test]
    fn test_mcp_tool_call_without_index_bootstraps_one() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("fresh.rs"), "pub fn fresh_fn() {}\n");
        let resp = call(dir.path(), 9, "lens_follow", json!({ "symbol": "fresh_fn" }));
        assert_jsonrpc_response(&resp, false);
        assert!(dir.path().join(".lens").join("index.db").exists(), "auto-bootstrap must create the index");
        assert!(text_of(&resp).contains("fresh_fn"));
    }

    #[test]
    fn test_mcp_tool_call_sees_edits_made_after_index() {
        let dir = project_with_index();
        // Bypass the freshness throttle for the test.
        std::env::set_var("LENS_FRESHNESS_THROTTLE_SECONDS", "0");
        write(&dir.path().join("b.rs"), "pub fn added_later() {}\n");
        let resp = call(dir.path(), 10, "lens_follow", json!({ "symbol": "added_later" }));
        std::env::remove_var("LENS_FRESHNESS_THROTTLE_SECONDS");
        assert_jsonrpc_response(&resp, false);
    }
}
