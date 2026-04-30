//! `lens mcp` — stdio MCP server.
//!
//! Implements a minimal subset of the Model Context Protocol over
//! newline-delimited JSON-RPC 2.0. Exposes the lens verbs (follow, refs,
//! query, explain, path, slice, map) as MCP tools so Claude Code can call
//! them directly via tool-use rather than shelling out via Bash.
//!
//! Protocol (subset):
//!   - `initialize` — handshake, returns server info + capabilities.
//!   - `initialized` (notification) — client signals readiness; no response.
//!   - `tools/list` — returns the tool catalogue with input JSON schemas.
//!   - `tools/call` — invokes a tool by name with arguments; returns
//!     `{ content: [{ type: "text", text }], isError }`.
//!   - `shutdown` / `exit` — graceful termination.
//!
//! Anything not in the subset above returns a `MethodNotFound` error.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use lens_core::{
    build_map, explain_symbol, follow_symbol, list_refs, query_graph, render_map,
    resolve_symbol_to_id, shortest_path, slice_at, Graph, Storage, TraversalMode,
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
pub fn run() -> Result<(), u8> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lens mcp: cannot resolve current directory: {e}");
            return Err(1);
        }
    };
    serve(BufReader::new(stdin.lock()), stdout.lock(), &cwd)
}

/// Pure I/O loop — separated from `run()` for testability. Reads lines from
/// `reader`, dispatches each as a JSON-RPC request, writes the response (if
/// any) to `writer`. Returns when EOF is reached or `exit` is processed.
pub fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    project_root: &Path,
) -> Result<(), u8> {
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
            // EOF.
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match handle_message(trimmed, project_root) {
            Some(r) => r,
            None => continue, // Notification (no response required).
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
        // `exit` is a graceful-shutdown notification — but we already returned
        // None above for notifications. The handler signals shutdown via a
        // result-flag we don't carry here; for v1, we rely on EOF from the
        // client closing stdin.
    }
}

/// Parse a single line and dispatch. Returns `Some(response)` for requests
/// (JSON-RPC messages with an `id`), `None` for notifications.
pub fn handle_message(line: &str, project_root: &Path) -> Option<Value> {
    // Parse — bad JSON returns a parse-error response keyed by null id.
    let raw: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                PARSE_ERROR,
                format!("parse error: {e}"),
            ));
        }
    };

    let id = raw.get("id").cloned();
    let method = match raw.get("method").and_then(|v| v.as_str()) {
        Some(m) => m.to_string(),
        None => {
            // Missing method on a request is an Invalid Request; on a
            // notification, we cannot respond either way → drop.
            if let Some(id) = id {
                return Some(error_response(
                    id,
                    INVALID_REQUEST,
                    "missing method".into(),
                ));
            }
            return None;
        }
    };
    let params = raw.get("params").cloned().unwrap_or(Value::Null);
    let is_notification = id.is_none();

    let result = dispatch(&method, &params, project_root);
    if is_notification {
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(McpError { code, message }) => error_response(id, code, message),
    })
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

fn dispatch(method: &str, params: &Value, project_root: &Path) -> Result<Value, McpError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
        })),
        "initialized" | "notifications/initialized" => Ok(Value::Null),
        "shutdown" => Ok(Value::Null),
        "tools/list" => Ok(json!({ "tools": tool_catalogue() })),
        "tools/call" => handle_tool_call(params, project_root),
        other => Err(McpError::method_not_found(format!("method not found: {other}"))),
    }
}

/// The catalogue of tools advertised to the client. Keep schemas minimal —
/// each tool's primary input is a single string plus a few numeric knobs.
pub fn tool_catalogue() -> Vec<Value> {
    vec![
        json!({
            "name": "lens_follow",
            "description": "Ctrl+Click — definition + signature + budget-fitted body slice + nearest callers for a symbol. Token-efficient replacement for reading whole files.",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name (qualified or unqualified)." },
                    "from":   { "type": "string", "description": "Disambiguation hint of the form FILE:LINE." },
                    "budget": { "type": "integer", "minimum": 0, "description": "Token budget for the body slice. Default 1500." }
                }
            }
        }),
        json!({
            "name": "lens_refs",
            "description": "List callers and reference sites of a symbol with file:line anchors. Use for impact analysis.",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": {
                    "symbol": { "type": "string" },
                    "limit":  { "type": "integer", "minimum": 1, "description": "Maximum reference sites to return. Default 20." }
                }
            }
        }),
        json!({
            "name": "lens_query",
            "description": "BFS/DFS query over the symbol graph. Returns budget-capped seeds for a topic.",
            "inputSchema": {
                "type": "object",
                "required": ["question"],
                "properties": {
                    "question": { "type": "string" },
                    "dfs":      { "type": "boolean", "description": "Depth-first traversal instead of breadth-first. Default false." },
                    "budget":   { "type": "integer", "minimum": 0, "description": "Token budget. Default 2000." }
                }
            }
        }),
        json!({
            "name": "lens_explain",
            "description": "Plain-language summary of a symbol and its neighbors (parents, children, callers, callees, types, imports).",
            "inputSchema": {
                "type": "object",
                "required": ["symbol"],
                "properties": { "symbol": { "type": "string" } }
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
                    "to":   { "type": "string" }
                }
            }
        }),
        json!({
            "name": "lens_slice",
            "description": "Minimal context slice for a file:line location, fitted to a token budget.",
            "inputSchema": {
                "type": "object",
                "required": ["location"],
                "properties": {
                    "location": { "type": "string", "description": "FILE:LINE form." },
                    "budget":   { "type": "integer", "minimum": 0, "description": "Token budget. Default 1500." }
                }
            }
        }),
        json!({
            "name": "lens_map",
            "description": "Architecture summary of the project (or a sub-tree). Aggregates files/symbols by directory with hot-spot detection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "Restrict to a sub-tree (project-relative path)." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Maximum traversal depth. Default 2." }
                }
            }
        }),
    ]
}

fn handle_tool_call(params: &Value, project_root: &Path) -> Result<Value, McpError> {
    #[derive(Deserialize)]
    struct CallParams {
        name: String,
        #[serde(default)]
        arguments: Value,
    }
    let cp: CallParams = serde_json::from_value(params.clone())
        .map_err(|e| McpError::invalid_params(format!("tools/call params: {e}")))?;

    let storage = open_storage(project_root)?;

    let text = match cp.name.as_str() {
        "lens_follow" => tool_follow(&cp.arguments, &storage, project_root)?,
        "lens_refs" => tool_refs(&cp.arguments, &storage)?,
        "lens_query" => tool_query(&cp.arguments, &storage)?,
        "lens_explain" => tool_explain(&cp.arguments, &storage)?,
        "lens_path" => tool_path(&cp.arguments, &storage)?,
        "lens_slice" => tool_slice(&cp.arguments, &storage, project_root)?,
        "lens_map" => tool_map(&cp.arguments, &storage)?,
        other => {
            return Err(McpError::invalid_params(format!(
                "unknown tool: {other}"
            )));
        }
    };

    // MCP `tools/call` result shape: a content array of typed blocks.
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn open_storage(project_root: &Path) -> Result<Storage, McpError> {
    let db_path: PathBuf = project_root.join(".lens").join("index.db");
    if !db_path.exists() {
        return Err(McpError::internal(format!(
            "lens index missing at {}; run `lens index` first.",
            db_path.display()
        )));
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
fn tool_follow(args: &Value, storage: &Storage, project_root: &Path) -> Result<String, McpError> {
    let a: FollowArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_follow: {e}")))?;
    let graph = Graph::load(storage).map_err(|e| McpError::internal(format!("graph: {e}")))?;
    let mut ids = resolve_symbol_to_id(&graph, &a.symbol);
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!(
            "no symbol matched '{}'", a.symbol
        )));
    }
    if let Some(from_str) = &a.from {
        if let Some((file, _line)) = parse_file_line(from_str) {
            ids.retain(|sid| {
                graph.symbols.get(sid).is_some_and(|m| m.file_path == file)
            });
            if ids.is_empty() {
                return Err(McpError::invalid_params(format!(
                    "--from '{from_str}' did not match any candidate"
                )));
            }
        }
    }
    if ids.len() > 1 {
        let mut msg = format!("'{}' is ambiguous ({} candidates):\n", a.symbol, ids.len());
        for sid in ids.iter().take(10) {
            if let Some(m) = graph.symbols.get(sid) {
                msg.push_str(&format!("  - {} ({} at {}:{})\n", m.qualified_name, m.kind, m.file_path, m.start_line));
            }
        }
        return Err(McpError::invalid_params(msg));
    }
    let result = follow_symbol(storage, project_root, ids[0], a.budget)
        .map_err(|e| McpError::internal(format!("follow: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("follow returned None for resolved id")))?;
    Ok(crate::cmd::follow::render_markdown(&a.symbol, &result))
}

#[derive(Deserialize)]
struct RefsArgs {
    symbol: String,
    #[serde(default = "default_limit_20")]
    limit: u32,
}
fn tool_refs(args: &Value, storage: &Storage) -> Result<String, McpError> {
    let a: RefsArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_refs: {e}")))?;
    let graph = Graph::load(storage).map_err(|e| McpError::internal(format!("graph: {e}")))?;
    let ids = resolve_symbol_to_id(&graph, &a.symbol);
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!("no symbol matched '{}'", a.symbol)));
    }
    if ids.len() > 1 {
        return Err(McpError::invalid_params(format!(
            "'{}' is ambiguous ({} candidates) — use a qualified name", a.symbol, ids.len()
        )));
    }
    let result = list_refs(storage, ids[0], a.limit)
        .map_err(|e| McpError::internal(format!("refs: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("refs returned None for resolved id")))?;
    Ok(crate::cmd::refs::render_markdown(&a.symbol, a.limit, &result))
}

#[derive(Deserialize)]
struct QueryArgs {
    question: String,
    #[serde(default)]
    dfs: bool,
    #[serde(default = "default_budget_2000")]
    budget: u32,
}
fn tool_query(args: &Value, storage: &Storage) -> Result<String, McpError> {
    let a: QueryArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_query: {e}")))?;
    let mode = if a.dfs { TraversalMode::Dfs } else { TraversalMode::Bfs };
    let result = query_graph(storage, &a.question, mode, a.budget)
        .map_err(|e| McpError::internal(format!("query: {e}")))?;
    Ok(crate::cmd::query::render_markdown(&a.question, &result))
}

#[derive(Deserialize)]
struct ExplainArgs { symbol: String }
fn tool_explain(args: &Value, storage: &Storage) -> Result<String, McpError> {
    let a: ExplainArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_explain: {e}")))?;
    let graph = Graph::load(storage).map_err(|e| McpError::internal(format!("graph: {e}")))?;
    let ids = resolve_symbol_to_id(&graph, &a.symbol);
    if ids.is_empty() {
        return Err(McpError::invalid_params(format!("no symbol matched '{}'", a.symbol)));
    }
    if ids.len() > 1 {
        return Err(McpError::invalid_params(format!(
            "'{}' is ambiguous ({} candidates)", a.symbol, ids.len()
        )));
    }
    let result = explain_symbol(&graph, ids[0])
        .map_err(|e| McpError::internal(format!("explain: {e}")))?
        .ok_or_else(|| McpError::internal(String::from("explain returned None for resolved id")))?;
    Ok(crate::cmd::explain::render_markdown(&result))
}

#[derive(Deserialize)]
struct PathArgs { from: String, to: String }
fn tool_path(args: &Value, storage: &Storage) -> Result<String, McpError> {
    let a: PathArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_path: {e}")))?;
    let graph = Graph::load(storage).map_err(|e| McpError::internal(format!("graph: {e}")))?;
    let from_ids = resolve_symbol_to_id(&graph, &a.from);
    let to_ids = resolve_symbol_to_id(&graph, &a.to);
    if from_ids.len() != 1 {
        return Err(McpError::invalid_params(format!(
            "'from' must resolve to exactly one symbol; got {}", from_ids.len()
        )));
    }
    if to_ids.len() != 1 {
        return Err(McpError::invalid_params(format!(
            "'to' must resolve to exactly one symbol; got {}", to_ids.len()
        )));
    }
    let result = shortest_path(&graph, from_ids[0], to_ids[0])
        .map_err(|e| McpError::internal(format!("path: {e}")))?;
    Ok(crate::cmd::path::render_markdown(&a.from, &a.to, result.as_ref()))
}

#[derive(Deserialize)]
struct SliceArgs {
    location: String,
    #[serde(default = "default_budget_1500")]
    budget: u32,
}
fn tool_slice(args: &Value, storage: &Storage, project_root: &Path) -> Result<String, McpError> {
    let a: SliceArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_slice: {e}")))?;
    let (file, line) = parse_file_line(&a.location)
        .ok_or_else(|| McpError::invalid_params(format!("location must be FILE:LINE; got '{}'", a.location)))?;
    let result = slice_at(storage, project_root, file, line, a.budget)
        .map_err(|e| McpError::internal(format!("slice: {e}")))?
        .ok_or_else(|| McpError::invalid_params(format!("no symbol covers {file}:{line}")))?;
    Ok(crate::cmd::slice::render_markdown(&a.location, &result))
}

#[derive(Deserialize)]
struct MapArgs {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default = "default_depth_2")]
    depth: u32,
}
fn tool_map(args: &Value, storage: &Storage) -> Result<String, McpError> {
    let a: MapArgs = serde_json::from_value(args.clone())
        .map_err(|e| McpError::invalid_params(format!("lens_map: {e}")))?;
    let result = build_map(storage, a.scope.as_deref(), a.depth)
        .map_err(|e| McpError::internal(format!("map: {e}")))?;
    Ok(render_map(&result))
}

fn parse_file_line(s: &str) -> Option<(&str, u32)> {
    let (file, line) = s.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    Some((file, line))
}

fn default_budget_1500() -> u32 { 1500 }
fn default_budget_2000() -> u32 { 2000 }
fn default_limit_20() -> u32 { 20 }
fn default_depth_2() -> u32 { 2 }

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
        let resp = handle_message(&req, dir.path());
        assert!(resp.is_none(), "notification must not produce a response");
    }

    #[test]
    fn test_mcp_tools_list_returns_all_seven_tools() {
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string();
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        let tools = resp.pointer("/result/tools").unwrap().as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.get("name").and_then(|n| n.as_str()).unwrap()).collect();
        assert!(names.contains(&"lens_follow"));
        assert!(names.contains(&"lens_refs"));
        assert!(names.contains(&"lens_query"));
        assert!(names.contains(&"lens_explain"));
        assert!(names.contains(&"lens_path"));
        assert!(names.contains(&"lens_slice"));
        assert!(names.contains(&"lens_map"));
    }

    #[test]
    fn test_mcp_tools_call_follow_returns_text_block() {
        let dir = project_with_index();
        let req = json!({
            "jsonrpc": "2.0", "id": 3,
            "method": "tools/call",
            "params": {
                "name": "lens_follow",
                "arguments": { "symbol": "target", "budget": 1000 }
            }
        }).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        assert_jsonrpc_response(&resp, false);
        let content = resp.pointer("/result/content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].get("type").and_then(|v| v.as_str()), Some("text"));
        let text = content[0].get("text").and_then(|v| v.as_str()).unwrap();
        assert!(text.contains("# Follow:") && text.contains("target"), "expected follow header in text: {text}");
    }

    #[test]
    fn test_mcp_tools_call_refs_returns_text_block() {
        let dir = project_with_index();
        let req = json!({
            "jsonrpc": "2.0", "id": 4,
            "method": "tools/call",
            "params": { "name": "lens_refs", "arguments": { "symbol": "target", "limit": 10 } }
        }).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        let text = resp.pointer("/result/content/0/text").and_then(|v| v.as_str()).unwrap();
        assert!(text.contains("# Refs:") && text.contains("target"), "expected refs header: {text}");
    }

    #[test]
    fn test_mcp_tools_call_map_returns_tree() {
        let dir = project_with_index();
        let req = json!({
            "jsonrpc": "2.0", "id": 5,
            "method": "tools/call",
            "params": { "name": "lens_map", "arguments": { "depth": 2 } }
        }).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        let text = resp.pointer("/result/content/0/text").and_then(|v| v.as_str()).unwrap();
        assert!(text.contains("# Map:"));
    }

    #[test]
    fn test_mcp_tools_call_unknown_tool_returns_invalid_params_error() {
        let dir = project_with_index();
        let req = json!({
            "jsonrpc": "2.0", "id": 6,
            "method": "tools/call",
            "params": { "name": "lens_nonexistent", "arguments": {} }
        }).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
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
        let input = "\n\n\n";
        let mut output: Vec<u8> = Vec::new();
        let r = serve(input.as_bytes(), &mut output, dir.path());
        assert_eq!(r, Ok(()));
        assert!(output.is_empty(), "blank lines should not produce output");
    }

    #[test]
    fn test_mcp_tool_call_without_index_returns_internal_error() {
        let dir = tempfile::tempdir().unwrap();
        let req = json!({
            "jsonrpc": "2.0", "id": 9,
            "method": "tools/call",
            "params": { "name": "lens_follow", "arguments": { "symbol": "x" } }
        }).to_string();
        let resp = handle_message(&req, dir.path()).expect("response expected");
        assert_eq!(resp.pointer("/error/code").and_then(|v| v.as_i64()), Some(INTERNAL_ERROR as i64));
        let msg = resp.pointer("/error/message").and_then(|v| v.as_str()).unwrap();
        assert!(msg.contains("lens index missing"), "expected guidance in error: {msg}");
    }
}
