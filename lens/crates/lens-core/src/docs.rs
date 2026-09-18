//! Any-language text index — the `docs` / `docs_fts` tables behind
//! `lens search` and the per-file token estimates behind the token meter.
//!
//! The symbol graph (`files` / `symbols` / `calls` / …) only covers the
//! languages that have a tree-sitter extractor. Real projects also contain
//! markdown, config, shell, SQL, templates, and source in languages lens
//! cannot parse. Claude still needs to *find* things in those files without
//! reading them whole. This module indexes every non-binary text file under
//! the project root into an FTS5 table so a keyword query returns
//! `file:line` hits (with the enclosing symbol when the file is also in the
//! symbol graph) instead of a `Grep` over the tree that floods the context.
//!
//! ## What gets indexed
//!
//! - Every regular file the `.gitignore`-aware walker yields, hidden files
//!   included, minus: `.git/`, `.lens/`, `.history/` (write-only archive by
//!   protocol), `node_modules/`, `__pycache__/`, `.venv/`/`venv/`, lock
//!   files, `.env*` (secrets), `.DS_Store`.
//! - Files larger than [`MAX_DOC_BYTES`] are skipped; files with a NUL byte
//!   in the first 8 KiB are treated as binary and skipped.
//! - The agent's state is always included even when gitignored:
//!   `.claude/state/**`, `current-tasks.md`, `schema.txt`, `CLAUDE.md`,
//!   `AGENTS.md`, `README.md` at the root. This is what makes lens double as
//!   a searchable memory for any model driving it over MCP.
//!
//! ## Incremental sync
//!
//! `docs` stores `(size_bytes, modified_at, content_hash)` per path. The
//! walker consults those stamps first: a file whose size and mtime match
//! is assumed unchanged and is *not read at all* — the freshness check that
//! runs before every read verb therefore costs one `stat` per file, not a
//! full read + hash. Only files whose stamp differs are read, hashed and
//! compared; a hash match after a stamp miss just refreshes the stamp.
//!
//! ## Search
//!
//! Queries are tokenised on whitespace. Each term becomes a quoted FTS5
//! prefix term (`"ensure"*`), so `ensure fre` matches `ensure_fresh`; bare
//! `OR` / `AND` / `NOT` pass through as operators. Files are ranked by
//! BM25; within a file, lines containing the most distinct terms come
//! first. Output is capped by a token budget computed with
//! [`crate::tokens::estimate_tokens`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use rusqlite::params;

use crate::error::{LensError, Result};
use crate::lang::Registry;
use crate::storage::insert::unix_seconds_now;
use crate::storage::Storage;
use crate::tokens::estimate_tokens;

/// Files above this size are not indexed for search (generated bundles,
/// fixtures, minified assets). 512 KiB comfortably covers hand-written
/// source and docs.
pub const MAX_DOC_BYTES: u64 = 512 * 1024;

/// Maximum hit lines reported per file by [`search_docs`].
pub const MAX_LINES_PER_FILE: usize = 6;

/// Default maximum number of files reported by [`search_docs`].
pub const DEFAULT_SEARCH_LIMIT: u32 = 20;

/// Characters kept per hit line before truncation with `…`.
const MAX_HIT_CHARS: usize = 160;

/// Directory names never descended into, regardless of `.gitignore`.
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".lens",
    ".history",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".gradle",
    ".dart_tool",
];

/// File basenames never indexed (lock files are large and content-free for
/// comprehension; `.env*` may hold secrets).
const EXCLUDED_FILES: &[&str] = &[
    ".DS_Store",
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
    "pubspec.lock",
    "flake.lock",
];

/// Root-relative paths (files or directories) indexed even when the
/// project's `.gitignore` excludes them. These are the agent's own memory.
const ALWAYS_INCLUDE: &[&str] = &[
    ".claude/state",
    "current-tasks.md",
    "schema.txt",
    "CLAUDE.md",
    "AGENTS.md",
    "README.md",
];

/// Stamp persisted per doc row; consulted by the walker to skip reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocStamp {
    pub size_bytes: u64,
    pub modified_at: i64,
    pub content_hash: [u8; 32],
}

/// One text file that needs (re-)indexing. Carries the body because the
/// FTS table stores content.
#[derive(Debug, Clone)]
pub struct DiscoveredDoc {
    pub relative_path: String,
    /// `"code"` when the extension has a registered extractor, else `"text"`.
    pub kind: &'static str,
    pub content_hash: [u8; 32],
    pub size_bytes: u64,
    pub modified_at: i64,
    pub body: String,
    pub token_estimate: u32,
    pub line_count: u32,
}

/// Result of a discovery pass.
#[derive(Debug, Default)]
pub struct DocsDiscovery {
    /// Files that are new or whose content hash changed — need upsert.
    pub to_upsert: Vec<DiscoveredDoc>,
    /// Files whose content is unchanged but whose stamp drifted (e.g. a
    /// `touch`) — only the stamp needs refreshing.
    pub restamp: Vec<(String, u64, i64)>,
    /// Every path seen on disk this pass; anything in the index but not here
    /// has been deleted.
    pub present: HashSet<String>,
    /// Files skipped as binary or oversized. Informational.
    pub skipped: u64,
}

/// Counts emitted by [`sync_docs`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DocsStats {
    pub added: u64,
    pub replaced: u64,
    pub deleted: u64,
    pub unchanged: u64,
    pub skipped: u64,
}

impl DocsStats {
    pub fn total_indexed(&self) -> u64 {
        self.added + self.replaced + self.unchanged
    }
}

/// Load `(path → stamp)` for every doc row. One scan; a few MB for a
/// 100K-file repo.
pub fn load_doc_stamps(storage: &Storage) -> Result<HashMap<String, DocStamp>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare("SELECT path, size_bytes, modified_at, content_hash FROM docs")
        .map_err(|e| LensError::other(format!("docs: prepare stamps: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|e| LensError::other(format!("docs: query stamps: {e}")))?;
    let mut map = HashMap::new();
    for r in rows {
        let (path, size, mtime, blob) = r.map_err(|e| LensError::other(format!("docs: row stamps: {e}")))?;
        if let Ok(hash) = <[u8; 32]>::try_from(blob.as_slice()) {
            map.insert(
                path,
                DocStamp { size_bytes: size.max(0) as u64, modified_at: mtime, content_hash: hash },
            );
        }
    }
    Ok(map)
}

/// Walk `root` and classify every text file against `known` stamps.
pub fn discover_docs(
    root: &Path,
    registry: &Registry,
    known: &HashMap<String, DocStamp>,
) -> Result<DocsDiscovery> {
    if !root.is_dir() {
        return Err(LensError::invalid_path(root, "not a directory"));
    }
    let mut out = DocsDiscovery::default();

    // Pass 1 — gitignore-aware walk over the whole tree, hidden entries
    // included, with the hard exclusion list applied via overrides.
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    for d in EXCLUDED_DIRS {
        overrides
            .add(&format!("!{d}/"))
            .map_err(|e| LensError::other(format!("docs: override {d}: {e}")))?;
    }
    for f in EXCLUDED_FILES {
        overrides
            .add(&format!("!{f}"))
            .map_err(|e| LensError::other(format!("docs: override {f}: {e}")))?;
    }
    overrides
        .add("!.env")
        .and_then(|b| b.add("!.env.*"))
        .map_err(|e| LensError::other(format!("docs: override .env: {e}")))?;
    let overrides = overrides
        .build()
        .map_err(|e| LensError::other(format!("docs: override build: {e}")))?;

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .overrides(overrides)
        .build();
    for entry in walker {
        let dent = match entry {
            Ok(d) => d,
            Err(e) => return Err(LensError::other(format!("docs: walk error: {e}"))),
        };
        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        consider(root, dent.path(), registry, known, &mut out)?;
    }

    // Pass 2 — always-include set, bypassing `.gitignore` (the agent's
    // state directory is commonly gitignored by the skill's own policy).
    for rel in ALWAYS_INCLUDE {
        let abs = root.join(rel);
        if abs.is_file() {
            consider(root, &abs, registry, known, &mut out)?;
        } else if abs.is_dir() {
            let walker = WalkBuilder::new(&abs)
                .hidden(false)
                .follow_links(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .ignore(false)
                .build();
            for entry in walker {
                let dent = match entry {
                    Ok(d) => d,
                    Err(e) => return Err(LensError::other(format!("docs: walk error: {e}"))),
                };
                if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                consider(root, dent.path(), registry, known, &mut out)?;
            }
        }
    }

    out.to_upsert.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    out.restamp.sort();
    Ok(out)
}

/// Classify one on-disk file. Skips duplicates (a path reached by both
/// passes), excluded basenames, oversized and binary files.
fn consider(
    root: &Path,
    abs: &Path,
    registry: &Registry,
    known: &HashMap<String, DocStamp>,
    out: &mut DocsDiscovery,
) -> Result<()> {
    let rel = match relative(root, abs) {
        Some(r) => r,
        None => return Ok(()),
    };
    if out.present.contains(&rel) {
        return Ok(());
    }
    if let Some(name) = abs.file_name().and_then(|n| n.to_str()) {
        if EXCLUDED_FILES.contains(&name) || name == ".env" || name.starts_with(".env.") {
            return Ok(());
        }
    }
    // Excluded directories can be reached through the always-include pass.
    if rel.split('/').any(|seg| EXCLUDED_DIRS.contains(&seg)) {
        return Ok(());
    }

    let metadata = match std::fs::metadata(abs) {
        Ok(m) => m,
        Err(_) => return Ok(()), // vanished between walk and stat — skip.
    };
    let size_bytes = metadata.len();
    if size_bytes > MAX_DOC_BYTES {
        out.skipped += 1;
        return Ok(());
    }
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Stat fast path: identical size + mtime ⇒ assume unchanged, no read.
    if let Some(stamp) = known.get(&rel) {
        if stamp.size_bytes == size_bytes && stamp.modified_at == modified_at {
            out.present.insert(rel);
            return Ok(());
        }
    }

    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    if is_binary(&bytes) {
        out.skipped += 1;
        return Ok(());
    }
    let content_hash = *blake3::hash(&bytes).as_bytes();
    out.present.insert(rel.clone());

    if let Some(stamp) = known.get(&rel) {
        if stamp.content_hash == content_hash {
            out.restamp.push((rel, size_bytes, modified_at));
            return Ok(());
        }
    }

    let body = String::from_utf8_lossy(&bytes).into_owned();
    let kind = match abs.extension().and_then(|e| e.to_str()) {
        Some(ext) if registry.language_for_extension(ext).is_some() => "code",
        _ => "text",
    };
    let token_estimate = estimate_tokens(&body);
    let line_count = body.lines().count() as u32;
    out.to_upsert.push(DiscoveredDoc {
        relative_path: rel,
        kind,
        content_hash,
        size_bytes,
        modified_at,
        body,
        token_estimate,
        line_count,
    });
    Ok(())
}

fn relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    let s = rel.to_str()?;
    if s.is_empty() {
        return None;
    }
    Some(s.replace('\\', "/"))
}

/// Binary heuristic: a NUL byte in the first 8 KiB.
pub fn is_binary(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(8192)];
    probe.contains(&0u8)
}

/// Walk, diff, and write the docs index for `root` in one pass. Safe to call
/// from `lens index`, `lens update`, and the auto-freshness check; the
/// common no-change case costs one `stat` per file and no writes.
pub fn sync_docs(storage: &mut Storage, root: &Path, registry: &Registry) -> Result<DocsStats> {
    let known = load_doc_stamps(storage)?;
    let discovery = discover_docs(root, registry, &known)?;
    apply_discovery(storage, &known, discovery)
}

/// Persist a discovery pass: upsert changed/new docs, refresh drifted
/// stamps, delete vanished paths. One transaction.
pub fn apply_discovery(
    storage: &mut Storage,
    known: &HashMap<String, DocStamp>,
    discovery: DocsDiscovery,
) -> Result<DocsStats> {
    let mut stats = DocsStats { skipped: discovery.skipped, ..Default::default() };
    let deleted: Vec<&String> = known
        .keys()
        .filter(|p| !discovery.present.contains(*p))
        .collect();
    let upsert_paths: HashSet<&str> = discovery.to_upsert.iter().map(|d| d.relative_path.as_str()).collect();
    stats.unchanged = discovery
        .present
        .iter()
        .filter(|p| known.contains_key(*p) && !upsert_paths.contains(p.as_str()))
        .count() as u64;

    if deleted.is_empty() && discovery.to_upsert.is_empty() && discovery.restamp.is_empty() {
        return Ok(stats);
    }

    let now = unix_seconds_now();
    let tx = storage.transaction()?;
    {
        let mut del_fts = tx
            .prepare("DELETE FROM docs_fts WHERE rowid = ?1")
            .map_err(|e| LensError::other(format!("docs: prepare delete fts: {e}")))?;
        let mut del_doc = tx
            .prepare("DELETE FROM docs WHERE id = ?1")
            .map_err(|e| LensError::other(format!("docs: prepare delete doc: {e}")))?;
        let mut find_id = tx
            .prepare("SELECT id FROM docs WHERE path = ?1")
            .map_err(|e| LensError::other(format!("docs: prepare find id: {e}")))?;
        let mut ins_doc = tx
            .prepare(
                "INSERT INTO docs (path, kind, content_hash, size_bytes, modified_at, indexed_at, token_estimate, line_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| LensError::other(format!("docs: prepare insert doc: {e}")))?;
        let mut ins_fts = tx
            .prepare("INSERT INTO docs_fts (rowid, path, body) VALUES (?1, ?2, ?3)")
            .map_err(|e| LensError::other(format!("docs: prepare insert fts: {e}")))?;
        let mut restamp = tx
            .prepare("UPDATE docs SET size_bytes = ?1, modified_at = ?2 WHERE path = ?3")
            .map_err(|e| LensError::other(format!("docs: prepare restamp: {e}")))?;

        let mut remove = |path: &str| -> Result<bool> {
            let id: Option<i64> = find_id
                .query_row(params![path], |r| r.get(0))
                .ok();
            if let Some(id) = id {
                del_fts
                    .execute(params![id])
                    .map_err(|e| LensError::other(format!("docs: delete fts {path}: {e}")))?;
                del_doc
                    .execute(params![id])
                    .map_err(|e| LensError::other(format!("docs: delete doc {path}: {e}")))?;
                return Ok(true);
            }
            Ok(false)
        };

        for path in deleted {
            if remove(path)? {
                stats.deleted += 1;
            }
        }
        for d in &discovery.to_upsert {
            if remove(&d.relative_path)? {
                stats.replaced += 1;
            } else {
                stats.added += 1;
            }
            ins_doc
                .execute(params![
                    &d.relative_path,
                    d.kind,
                    &d.content_hash[..],
                    d.size_bytes as i64,
                    d.modified_at,
                    now,
                    d.token_estimate as i64,
                    d.line_count as i64,
                ])
                .map_err(|e| LensError::other(format!("docs: insert {}: {e}", d.relative_path)))?;
            let id = tx.last_insert_rowid();
            ins_fts
                .execute(params![id, &d.relative_path, &d.body])
                .map_err(|e| LensError::other(format!("docs: insert fts {}: {e}", d.relative_path)))?;
        }
        for (path, size, mtime) in &discovery.restamp {
            restamp
                .execute(params![*size as i64, *mtime, path])
                .map_err(|e| LensError::other(format!("docs: restamp {path}: {e}")))?;
        }
    }
    tx.commit()
        .map_err(|e| LensError::other(format!("docs: commit: {e}")))?;
    Ok(stats)
}

/// Per-file token estimate for `path`, from the docs index when present,
/// else by reading the file. `None` when neither works.
pub fn file_token_estimate(storage: &Storage, root: &Path, path: &str) -> Option<u32> {
    let indexed: Option<i64> = storage
        .connection()
        .query_row(
            "SELECT token_estimate FROM docs WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )
        .ok();
    if let Some(t) = indexed {
        return Some(t.max(0) as u32);
    }
    let text = std::fs::read_to_string(root.join(path)).ok()?;
    Some(estimate_tokens(&text))
}

/// Sum of per-file token estimates over distinct `paths` — what a model
/// would spend reading those files whole.
pub fn total_file_tokens<'a, I: IntoIterator<Item = &'a str>>(
    storage: &Storage,
    root: &Path,
    paths: I,
) -> u64 {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut total: u64 = 0;
    for p in paths {
        if seen.insert(p) {
            if let Some(t) = file_token_estimate(storage, root, p) {
                total += t as u64;
            }
        }
    }
    total
}

// ----- Search ---------------------------------------------------------------

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// 1-indexed line number.
    pub line: u32,
    /// Trimmed line text, truncated to [`MAX_HIT_CHARS`].
    pub text: String,
    /// Enclosing symbol `(qualified_name, kind, start_line)` when the file
    /// is in the symbol graph and a symbol spans this line.
    pub symbol: Option<(String, String, i64)>,
}

/// All hits for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFile {
    pub path: String,
    pub kind: String,
    pub hits: Vec<SearchHit>,
    /// Total matching lines in the file (may exceed `hits.len()`).
    pub matching_lines: u32,
}

/// What [`search_docs`] returns.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub query: String,
    /// Lower-cased terms actually matched against lines.
    pub terms: Vec<String>,
    pub files: Vec<SearchFile>,
    /// Files matched by FTS before the `limit` / budget cut.
    pub files_matched: u64,
    /// True when files or lines were dropped for `limit` or budget.
    pub truncated: bool,
    pub budget: u32,
    pub estimated_tokens: u32,
}

/// Options for [`search_docs`].
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Maximum files to report.
    pub limit: u32,
    /// Token budget for the rendered result.
    pub budget: u32,
    /// Restrict to paths equal to or under this project-relative prefix.
    pub scope: Option<String>,
    /// `Some("code")` / `Some("text")` to restrict by kind.
    pub kind: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { limit: DEFAULT_SEARCH_LIMIT, budget: 2000, scope: None, kind: None }
    }
}

/// Approximate rendered cost of a file header and of one hit line, used for
/// budget accounting. Renderers should stay close to these shapes.
const FILE_HEADER_TOKENS: u32 = 12;
const HIT_OVERHEAD_TOKENS: u32 = 10;

/// Build the FTS5 MATCH expression for a free-text query. Public for tests.
pub fn build_match(query: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        if matches!(raw, "OR" | "AND" | "NOT") {
            parts.push(raw.to_string());
            continue;
        }
        let cleaned: String = raw.chars().filter(|c| *c != '"' && *c != '*').collect();
        if cleaned.is_empty() {
            continue;
        }
        parts.push(format!("\"{cleaned}\"*"));
    }
    // Drop dangling operators at either end.
    while parts.first().is_some_and(|p| matches!(p.as_str(), "OR" | "AND")) {
        parts.remove(0);
    }
    while parts.last().is_some_and(|p| matches!(p.as_str(), "OR" | "AND" | "NOT")) {
        parts.pop();
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// Lower-cased plain terms (operators and quotes removed) used for the
/// per-line scan.
fn plain_terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        if matches!(raw, "OR" | "AND" | "NOT") {
            continue;
        }
        let cleaned: String = raw
            .chars()
            .filter(|c| *c != '"' && *c != '*')
            .collect::<String>()
            .to_lowercase();
        if !cleaned.is_empty() && !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    out
}

/// Full-text search over the docs index.
pub fn search_docs(storage: &Storage, query: &str, opts: &SearchOptions) -> Result<SearchResult> {
    let mut result = SearchResult {
        query: query.to_string(),
        terms: plain_terms(query),
        budget: opts.budget,
        ..Default::default()
    };
    let Some(match_expr) = build_match(query) else {
        return Ok(result);
    };

    let scope = opts.scope.as_deref().map(|s| s.trim_matches('/').to_string()).filter(|s| !s.is_empty());
    let kind = opts.kind.clone().filter(|k| k == "code" || k == "text");

    // Rank files by BM25. We over-fetch (limit + 1) to detect truncation
    // without a COUNT, and additionally count all matches cheaply.
    let conn = storage.connection();
    let mut sql = String::from(
        "SELECT d.id, d.path, d.kind, f.body
         FROM docs_fts f JOIN docs d ON d.id = f.rowid
         WHERE docs_fts MATCH ?1",
    );
    if scope.is_some() {
        sql.push_str(" AND (d.path = ?2 OR d.path LIKE ?3)");
    }
    if kind.is_some() {
        sql.push_str(if scope.is_some() { " AND d.kind = ?4" } else { " AND d.kind = ?2" });
    }
    sql.push_str(" ORDER BY rank LIMIT ?");
    let limit_pos = 1 + if scope.is_some() { 2 } else { 0 } + if kind.is_some() { 1 } else { 0 } + 1;
    sql.push_str(&limit_pos.to_string());

    let scope_like: String = scope.as_ref().map(|s| format!("{s}/%")).unwrap_or_default();
    let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&match_expr];
    if let Some(s) = scope.as_ref() {
        bind.push(s);
        bind.push(&scope_like);
    }
    if let Some(k) = kind.as_ref() {
        bind.push(k);
    }
    let fetch_limit = (opts.limit as i64).saturating_add(1);
    bind.push(&fetch_limit);

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| LensError::other(format!("search: prepare: {e}")))?;
    let rows: Vec<(i64, String, String, String)> = stmt
        .query_map(bind.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| LensError::other(format!("search: query: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("search: collect: {e}")))?;

    result.files_matched = rows.len() as u64;
    if rows.len() as u32 > opts.limit {
        result.truncated = true;
    }

    let mut sym_stmt = conn
        .prepare(
            "SELECT s.qualified_name, s.kind, s.start_line
             FROM symbols s JOIN files fl ON fl.id = s.file_id
             WHERE fl.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
             ORDER BY (s.end_line - s.start_line) ASC, s.id ASC
             LIMIT 1",
        )
        .map_err(|e| LensError::other(format!("search: prepare symbol lookup: {e}")))?;

    let mut used: u32 = 0;
    for (_id, path, file_kind, body) in rows.into_iter().take(opts.limit as usize) {
        // Score lines by distinct-term coverage.
        let mut scored: Vec<(usize, u32, &str)> = Vec::new();
        for (idx, line) in body.lines().enumerate() {
            let lower = line.to_lowercase();
            let n = result.terms.iter().filter(|t| lower.contains(t.as_str())).count() as u32;
            if n > 0 {
                scored.push((idx, n, line));
            }
        }
        if scored.is_empty() {
            // FTS matched via tokenisation the substring scan cannot see
            // (e.g. diacritics folding). Report the file without lines.
            scored.push((0, 0, ""));
        }
        let matching_lines = scored.iter().filter(|s| s.1 > 0).count() as u32;
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        if used.saturating_add(FILE_HEADER_TOKENS) > opts.budget {
            result.truncated = true;
            break;
        }
        used = used.saturating_add(FILE_HEADER_TOKENS);

        let mut hits: Vec<SearchHit> = Vec::new();
        for (idx, n, line) in scored.into_iter().take(MAX_LINES_PER_FILE) {
            if n == 0 {
                break;
            }
            let text = clip(line.trim());
            let line_no = (idx + 1) as u32;
            let symbol = if file_kind == "code" {
                sym_stmt
                    .query_row(params![&path, line_no as i64], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                    })
                    .ok()
            } else {
                None
            };
            let cost = estimate_tokens(&text)
                .saturating_add(HIT_OVERHEAD_TOKENS)
                .saturating_add(symbol.as_ref().map(|s| estimate_tokens(&s.0)).unwrap_or(0));
            if used.saturating_add(cost) > opts.budget {
                result.truncated = true;
                break;
            }
            used = used.saturating_add(cost);
            hits.push(SearchHit { line: line_no, text, symbol });
        }
        if hits.len() < matching_lines as usize {
            result.truncated = true;
        }
        result.files.push(SearchFile { path, kind: file_kind, hits, matching_lines });
    }
    result.estimated_tokens = used;
    Ok(result)
}

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_HIT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_HIT_CHARS).collect();
    out.push('…');
    out
}

/// Absolute path helper used by the CLI to resolve a `--scope` argument.
pub fn scope_to_relative(root: &Path, scope: &Path) -> Option<String> {
    let rel: PathBuf = if scope.is_absolute() {
        scope.strip_prefix(root).ok()?.to_path_buf()
    } else {
        scope.to_path_buf()
    };
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn project() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(".lens").join("index.db");
        let storage = Storage::open(&db).unwrap();
        (dir, storage)
    }

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    fn count_docs(s: &Storage) -> i64 {
        s.connection().query_row("SELECT COUNT(*) FROM docs", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn test_docs_sync_indexes_code_and_text_files() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "src/a.rs", b"pub fn ensure_fresh() {}\n");
        write(root, "notes/design.md", b"# Design\nfreshness throttle is five seconds\n");
        let reg = Registry::with_default_languages();
        let stats = sync_docs(&mut s, root, &reg).unwrap();
        assert_eq!(stats.added, 2);
        assert_eq!(count_docs(&s), 2);
        let kinds: Vec<(String, String)> = s
            .connection()
            .prepare("SELECT path, kind FROM docs ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(kinds, vec![("notes/design.md".into(), "text".into()), ("src/a.rs".into(), "code".into())]);
    }

    #[test]
    fn test_docs_sync_skips_binary_and_oversized_files() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "bin.dat", &[0u8, 1, 2, 3, 0, 5]);
        let big = vec![b'a'; (MAX_DOC_BYTES + 1) as usize];
        write(root, "big.txt", &big);
        write(root, "ok.txt", b"hello");
        let reg = Registry::with_default_languages();
        let stats = sync_docs(&mut s, root, &reg).unwrap();
        assert_eq!(stats.added, 1);
        assert_eq!(stats.skipped, 2);
    }

    #[test]
    fn test_docs_sync_excludes_lens_git_history_and_env_files() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, ".git/config", b"[core]\n");
        write(root, ".lens/config.toml", b"[index]\n");
        write(root, ".history/2026-01-01/a.rs", b"old\n");
        write(root, ".env", b"SECRET=1\n");
        write(root, ".env.local", b"SECRET=2\n");
        write(root, "node_modules/x/index.js", b"module.exports = 1\n");
        write(root, "Cargo.lock", b"[[package]]\n");
        write(root, "keep.txt", b"keep\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let paths: Vec<String> = s
            .connection()
            .prepare("SELECT path FROM docs ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(paths, vec!["keep.txt"]);
    }

    #[test]
    fn test_docs_sync_always_includes_agent_state_even_when_gitignored() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, ".gitignore", b".claude/state/\n*.md\n");
        write(root, ".claude/state/code-map/area.md", b"# code-map: area\n");
        write(root, "current-tasks.md", b"# Current Tasks\n");
        write(root, "ignored/notes.md", b"should be ignored\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let paths: Vec<String> = s
            .connection()
            .prepare("SELECT path FROM docs ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(paths.contains(&".claude/state/code-map/area.md".to_string()), "{paths:?}");
        assert!(paths.contains(&"current-tasks.md".to_string()), "{paths:?}");
        assert!(!paths.contains(&"ignored/notes.md".to_string()), "{paths:?}");
    }

    #[test]
    fn test_docs_sync_second_run_is_noop_via_stat_fast_path() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.txt", b"one\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let known = load_doc_stamps(&s).unwrap();
        let disc = discover_docs(root, &reg, &known).unwrap();
        assert!(disc.to_upsert.is_empty());
        assert!(disc.restamp.is_empty());
        assert_eq!(disc.present.len(), 1);
        let stats = apply_discovery(&mut s, &known, disc).unwrap();
        assert_eq!(stats.unchanged, 1);
        assert_eq!(stats.added + stats.replaced + stats.deleted, 0);
    }

    #[test]
    fn test_docs_sync_detects_change_and_delete() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.txt", b"one\n");
        write(root, "b.txt", b"two\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        // Force a stamp miss regardless of mtime granularity by changing size.
        write(root, "a.txt", b"one two three\n");
        fs::remove_file(root.join("b.txt")).unwrap();
        let stats = sync_docs(&mut s, root, &reg).unwrap();
        assert_eq!(stats.replaced, 1);
        assert_eq!(stats.deleted, 1);
        assert_eq!(count_docs(&s), 1);
        let fts_rows: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM docs_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_rows, 1, "fts rows must track docs rows");
    }

    #[test]
    fn test_build_match_quotes_and_prefixes_terms() {
        assert_eq!(build_match("ensure fresh").as_deref(), Some("\"ensure\"* \"fresh\"*"));
        assert_eq!(build_match("a OR b").as_deref(), Some("\"a\"* OR \"b\"*"));
        assert_eq!(build_match("auto-freshness").as_deref(), Some("\"auto-freshness\"*"));
        assert_eq!(build_match("   "), None);
        assert_eq!(build_match("OR").as_deref(), None);
        assert_eq!(build_match("\"quoted\"*").as_deref(), Some("\"quoted\"*"));
    }

    #[test]
    fn test_search_returns_file_line_hits_with_enclosing_symbol() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            b"/// Keeps the index fresh.\npub fn ensure_fresh() {\n    let throttle = 5;\n    let _ = throttle;\n}\n",
        );
        write(root, "docs/guide.md", b"# Guide\n\nRun ensure_fresh before reads.\n");
        let reg = Registry::with_default_languages();
        // Symbol graph for the code file, then docs.
        let files = crate::extract::run(root, &reg).unwrap();
        crate::storage::insert_extracted_files(&mut s, &files).unwrap();
        sync_docs(&mut s, root, &reg).unwrap();

        let r = search_docs(&s, "ensure_fresh", &SearchOptions::default()).unwrap();
        assert_eq!(r.files_matched, 2);
        let code = r.files.iter().find(|f| f.path == "src/lib.rs").expect("code file hit");
        assert_eq!(code.kind, "code");
        assert!(code.hits.iter().any(|h| h.line == 2), "{:?}", code.hits);
        let hit = code.hits.iter().find(|h| h.line == 2).unwrap();
        let (qname, kind, _) = hit.symbol.as_ref().expect("enclosing symbol");
        assert!(qname.ends_with("ensure_fresh"), "{qname}");
        assert_eq!(kind, "function");
        let doc = r.files.iter().find(|f| f.path == "docs/guide.md").expect("doc hit");
        assert_eq!(doc.kind, "text");
        assert_eq!(doc.hits[0].line, 3);
        assert!(doc.hits[0].symbol.is_none());
    }

    #[test]
    fn test_search_prefix_matches_partial_identifier() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.py", b"def compute_checksum(data):\n    return 1\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let r = search_docs(&s, "compute_check", &SearchOptions::default()).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].hits[0].line, 1);
    }

    #[test]
    fn test_search_respects_scope_kind_and_limit() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "src/a.rs", b"fn needle() {}\n");
        write(root, "tests/b.rs", b"fn needle_test() {}\n");
        write(root, "README.md", b"needle in docs\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();

        let scoped = search_docs(
            &s,
            "needle",
            &SearchOptions { scope: Some("src".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(scoped.files.len(), 1);
        assert_eq!(scoped.files[0].path, "src/a.rs");

        let text_only = search_docs(
            &s,
            "needle",
            &SearchOptions { kind: Some("text".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(text_only.files.len(), 1);
        assert_eq!(text_only.files[0].path, "README.md");

        let limited = search_docs(&s, "needle", &SearchOptions { limit: 1, ..Default::default() }).unwrap();
        assert_eq!(limited.files.len(), 1);
        assert!(limited.truncated);
    }

    #[test]
    fn test_search_budget_truncates_hits() {
        let (dir, mut s) = project();
        let root = dir.path();
        let body: String = (0..40).map(|i| format!("needle line {i} with some words\n")).collect();
        write(root, "big.txt", body.as_bytes());
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let r = search_docs(&s, "needle", &SearchOptions { budget: 40, ..Default::default() }).unwrap();
        assert!(r.truncated);
        assert!(r.estimated_tokens <= 40);
        assert!(r.files[0].hits.len() < MAX_LINES_PER_FILE);
    }

    #[test]
    fn test_search_no_match_returns_empty_without_error() {
        let (dir, mut s) = project();
        write(dir.path(), "a.txt", b"nothing here\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, dir.path(), &reg).unwrap();
        let r = search_docs(&s, "zzz_not_present", &SearchOptions::default()).unwrap();
        assert!(r.files.is_empty());
        assert_eq!(r.files_matched, 0);
    }

    #[test]
    fn test_file_token_estimate_prefers_index_and_falls_back_to_disk() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.txt", b"one two three\n");
        write(root, "b.txt", b"four five six seven\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        // Remove b from the index so the fallback path runs.
        s.connection().execute("DELETE FROM docs WHERE path = 'b.txt'", []).unwrap();
        assert_eq!(file_token_estimate(&s, root, "a.txt"), Some(estimate_tokens("one two three\n")));
        assert_eq!(file_token_estimate(&s, root, "b.txt"), Some(estimate_tokens("four five six seven\n")));
        assert_eq!(file_token_estimate(&s, root, "missing.txt"), None);
        assert_eq!(
            total_file_tokens(&s, root, ["a.txt", "a.txt", "b.txt"]),
            (estimate_tokens("one two three\n") + estimate_tokens("four five six seven\n")) as u64
        );
    }

    #[test]
    fn test_is_binary_detects_nul_only() {
        assert!(is_binary(&[b'a', 0, b'b']));
        assert!(!is_binary(b"plain text\n"));
        assert!(!is_binary(b""));
    }
}
