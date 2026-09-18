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
use rusqlite::{params, OptionalExtension};

use crate::assets::{self, AssetMeta};
use crate::error::{LensError, Result};
use crate::lang::Registry;
use crate::storage::insert::unix_seconds_now;
use crate::storage::Storage;
use crate::tokens::estimate_tokens;

/// Kinds that carry an `assets` row (everything that is not code/text).
pub const ASSET_KINDS: &[&str] = &["image", "video", "audio", "pdf", "office", "archive", "binary"];

/// All kinds accepted by `--kind` filters.
pub const ALL_KINDS: &[&str] = &["code", "text", "image", "video", "audio", "pdf", "office", "archive", "binary"];

/// Binary files above this size get a metadata-only row (header probe);
/// they are never read whole.
pub const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

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

/// One file that needs (re-)indexing. Carries the body because the FTS
/// table stores content. For assets the body is the searchable summary +
/// extracted text; the stored description is merged in at apply time.
#[derive(Debug, Clone)]
pub struct DiscoveredDoc {
    pub relative_path: String,
    /// `"code"` when the extension has a registered extractor, `"text"` for
    /// other readable files, else an asset kind (`image`, `pdf`, …).
    pub kind: &'static str,
    pub content_hash: [u8; 32],
    pub size_bytes: u64,
    pub modified_at: i64,
    pub body: String,
    pub token_estimate: u32,
    pub line_count: u32,
    /// Present for asset kinds.
    pub asset: Option<AssetMeta>,
}

/// A stored asset record — what `lens describe <file>` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRecord {
    pub path: String,
    pub kind: String,
    pub mime: String,
    pub size_bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub pages: Option<u32>,
    pub extracted_chars: u64,
    pub extractor: String,
    pub description: Option<String>,
    pub described_by: Option<String>,
    pub described_at: Option<i64>,
    /// The searchable body (summary + description + extracted text).
    pub body: String,
    pub token_estimate: u32,
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
    let ext = abs.extension().and_then(|e| e.to_str()).unwrap_or("");
    let known_asset = assets::kind_for_extension(ext).0 != "binary";
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

    // Asset path: known media/document extensions, or content that sniffs
    // as binary. Identity is blake3(first 1 MiB || size) so multi-GB media
    // is never read whole. Oversized *text* files are skipped as before.
    let prefix = probe_prefix(abs);
    let is_asset = known_asset || is_binary(&prefix);
    if !is_asset && size_bytes > MAX_DOC_BYTES {
        out.skipped += 1;
        return Ok(());
    }
    if is_asset {
        if size_bytes > MAX_ASSET_BYTES {
            out.skipped += 1;
            return Ok(());
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&prefix);
        hasher.update(&size_bytes.to_le_bytes());
        let content_hash = *hasher.finalize().as_bytes();
        out.present.insert(rel.clone());
        if let Some(stamp) = known.get(&rel) {
            if stamp.content_hash == content_hash {
                out.restamp.push((rel, size_bytes, modified_at));
                return Ok(());
            }
        }
        let meta = assets::extract(abs, size_bytes);
        let kind: &'static str = meta.kind;
        let body = assets::build_body(&meta, None);
        let token_estimate = assets::body_tokens(&body);
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
            asset: Some(meta),
        });
        return Ok(());
    }

    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
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
        asset: None,
    });
    Ok(())
}

/// First [`assets::HEADER_PROBE_BYTES`] of a file (or the whole file when
/// smaller). Used for the binary sniff and the asset identity hash.
fn probe_prefix(abs: &Path) -> Vec<u8> {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(abs) else { return Vec::new() };
    let mut buf = Vec::new();
    let _ = f.take(assets::HEADER_PROBE_BYTES as u64).read_to_end(&mut buf);
    buf
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
        let mut get_desc = tx
            .prepare("SELECT a.description, a.described_by, a.described_at FROM assets a JOIN docs d ON d.id = a.doc_id WHERE d.path = ?1")
            .map_err(|e| LensError::other(format!("docs: prepare get description: {e}")))?;
        let mut ins_asset = tx
            .prepare(
                "INSERT INTO assets (doc_id, mime, width, height, duration_ms, pages, extracted_chars, extractor, description, described_by, described_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .map_err(|e| LensError::other(format!("docs: prepare insert asset: {e}")))?;

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
            // A stored description outlives the bytes it described — carry
            // it over before the CASCADE delete drops the old asset row.
            let carried: Option<(Option<String>, Option<String>, Option<i64>)> = if d.asset.is_some() {
                get_desc
                    .query_row(params![&d.relative_path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .ok()
            } else {
                None
            };
            if remove(&d.relative_path)? {
                stats.replaced += 1;
            } else {
                stats.added += 1;
            }
            let (body, token_estimate, line_count) = match (&d.asset, &carried) {
                (Some(meta), Some((Some(desc), _, _))) => {
                    let b = assets::build_body(meta, Some(desc));
                    let t = assets::body_tokens(&b);
                    let l = b.lines().count() as u32;
                    (b, t, l)
                }
                _ => (d.body.clone(), d.token_estimate, d.line_count),
            };
            ins_doc
                .execute(params![
                    &d.relative_path,
                    d.kind,
                    &d.content_hash[..],
                    d.size_bytes as i64,
                    d.modified_at,
                    now,
                    token_estimate as i64,
                    line_count as i64,
                ])
                .map_err(|e| LensError::other(format!("docs: insert {}: {e}", d.relative_path)))?;
            let id = tx.last_insert_rowid();
            ins_fts
                .execute(params![id, &d.relative_path, &body])
                .map_err(|e| LensError::other(format!("docs: insert fts {}: {e}", d.relative_path)))?;
            if let Some(meta) = &d.asset {
                let (desc, by, at) = carried.unwrap_or((None, None, None));
                ins_asset
                    .execute(params![
                        id,
                        &meta.mime,
                        meta.width.map(|v| v as i64),
                        meta.height.map(|v| v as i64),
                        meta.duration_ms.map(|v| v as i64),
                        meta.pages.map(|v| v as i64),
                        meta.extracted_text.chars().count() as i64,
                        meta.extractor,
                        desc,
                        by,
                        at,
                    ])
                    .map_err(|e| LensError::other(format!("docs: insert asset {}: {e}", d.relative_path)))?;
            }
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

/// Read the stored asset record for `path`. `Ok(None)` when the path is
/// not an indexed asset (a text file, or not indexed at all).
pub fn describe(storage: &Storage, path: &str) -> Result<Option<AssetRecord>> {
    let conn = storage.connection();
    let row = conn
        .query_row(
            "SELECT d.id, d.kind, d.size_bytes, d.token_estimate, a.mime, a.width, a.height, a.duration_ms, a.pages,
                    a.extracted_chars, a.extractor, a.description, a.described_by, a.described_at,
                    (SELECT body FROM docs_fts WHERE rowid = d.id)
             FROM docs d JOIN assets a ON a.doc_id = d.id WHERE d.path = ?1",
            params![path],
            |r| {
                Ok(AssetRecord {
                    path: path.to_string(),
                    kind: r.get(1)?,
                    size_bytes: r.get::<_, i64>(2)?.max(0) as u64,
                    token_estimate: r.get::<_, i64>(3)?.max(0) as u32,
                    mime: r.get(4)?,
                    width: r.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                    height: r.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    duration_ms: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                    pages: r.get::<_, Option<i64>>(8)?.map(|v| v as u32),
                    extracted_chars: r.get::<_, i64>(9)?.max(0) as u64,
                    extractor: r.get(10)?,
                    description: r.get(11)?,
                    described_by: r.get(12)?,
                    described_at: r.get(13)?,
                    body: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(|e| LensError::other(format!("describe: query: {e}")))?;
    Ok(row)
}

/// Store (or clear, with `None`) the description of an indexed asset and
/// rebuild its searchable body. Returns `Ok(false)` when `path` is not an
/// indexed asset.
pub fn set_description(
    storage: &mut Storage,
    path: &str,
    description: Option<&str>,
    by: Option<&str>,
) -> Result<bool> {
    let Some(rec) = describe(storage, path)? else { return Ok(false) };
    // Rebuild the body from the stored metadata + extracted text (kept in
    // the FTS body after the summary/description lines).
    let extracted = extracted_text_of(&rec);
    let meta = AssetMeta {
        kind: ASSET_KINDS.iter().copied().find(|k| *k == rec.kind).unwrap_or("binary"),
        mime: rec.mime.clone(),
        width: rec.width,
        height: rec.height,
        duration_ms: rec.duration_ms,
        pages: rec.pages,
        extracted_text: extracted,
        extractor: "stored",
    };
    let desc = description.map(str::trim).filter(|d| !d.is_empty());
    let body = assets::build_body(&meta, desc);
    let tokens = assets::body_tokens(&body) as i64;
    let lines = body.lines().count() as i64;
    let now = unix_seconds_now();
    let tx = storage.transaction()?;
    let id: i64 = tx
        .query_row("SELECT id FROM docs WHERE path = ?1", params![path], |r| r.get(0))
        .map_err(|e| LensError::other(format!("describe: id: {e}")))?;
    tx.execute(
        "UPDATE assets SET description = ?1, described_by = ?2, described_at = ?3 WHERE doc_id = ?4",
        params![desc, desc.and(by), desc.map(|_| now), id],
    )
    .map_err(|e| LensError::other(format!("describe: update asset: {e}")))?;
    tx.execute(
        "UPDATE docs SET token_estimate = ?1, line_count = ?2 WHERE id = ?3",
        params![tokens, lines, id],
    )
    .map_err(|e| LensError::other(format!("describe: update doc: {e}")))?;
    tx.execute("DELETE FROM docs_fts WHERE rowid = ?1", params![id])
        .map_err(|e| LensError::other(format!("describe: delete fts: {e}")))?;
    tx.execute("INSERT INTO docs_fts (rowid, path, body) VALUES (?1, ?2, ?3)", params![id, path, &body])
        .map_err(|e| LensError::other(format!("describe: insert fts: {e}")))?;
    tx.commit().map_err(|e| LensError::other(format!("describe: commit: {e}")))?;
    Ok(true)
}

/// The extracted-text part of a stored body: everything after the summary
/// line and the optional `description:` line.
fn extracted_text_of(rec: &AssetRecord) -> String {
    let mut lines = rec.body.lines();
    let _summary = lines.next();
    let mut rest: Vec<&str> = lines.collect();
    if rest.first().is_some_and(|l| l.starts_with("description: ")) {
        rest.remove(0);
    }
    rest.join("\n")
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
    /// True when no file matched *all* terms and the query was re-run as
    /// `term OR term …` (files matching any term, ranked by how many).
    pub relaxed: bool,
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
    let kind = opts.kind.clone().filter(|k| ALL_KINDS.contains(&k.as_str()));

    // Strict pass (all terms), then — for multi-term queries without
    // explicit operators — a relaxed OR pass so a model asking about two
    // topics at once still gets the best files for either.
    let has_operators = query.split_whitespace().any(|t| matches!(t, "OR" | "AND" | "NOT"));
    let mut rows = fetch_ranked(storage, &match_expr, scope.as_deref(), kind.as_deref(), opts.limit)?;
    if rows.is_empty() && result.terms.len() > 1 && !has_operators {
        let relaxed_expr = result.terms.iter().map(|t| format!("\"{t}\"*")).collect::<Vec<_>>().join(" OR ");
        rows = fetch_ranked(storage, &relaxed_expr, scope.as_deref(), kind.as_deref(), opts.limit)?;
        result.relaxed = !rows.is_empty();
    }
    finish_search(storage, opts, result, rows)
}

/// FTS query + scope/kind filters, over-fetching by one to detect truncation.
fn fetch_ranked(
    storage: &Storage,
    match_expr: &str,
    scope: Option<&str>,
    kind: Option<&str>,
    limit: u32,
) -> Result<Vec<(i64, String, String, String)>> {

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

    let scope_like: String = scope.map(|s| format!("{s}/%")).unwrap_or_default();
    let scope_owned: String = scope.unwrap_or_default().to_string();
    let kind_owned: String = kind.unwrap_or_default().to_string();
    let match_owned = match_expr.to_string();
    let mut bind: Vec<&dyn rusqlite::ToSql> = vec![&match_owned];
    if scope.is_some() {
        bind.push(&scope_owned);
        bind.push(&scope_like);
    }
    if kind.is_some() {
        bind.push(&kind_owned);
    }
    let fetch_limit = (limit as i64).saturating_add(1);
    bind.push(&fetch_limit);

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| LensError::other(format!("search: prepare: {e}")))?;
    let rows = stmt
        .query_map(bind.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| LensError::other(format!("search: query: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LensError::other(format!("search: collect: {e}")))?;
    Ok(rows)
}

/// Turn ranked rows into per-line hits under the token budget.
fn finish_search(
    storage: &Storage,
    opts: &SearchOptions,
    mut result: SearchResult,
    rows: Vec<(i64, String, String, String)>,
) -> Result<SearchResult> {
    let conn = storage.connection();
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
    fn test_docs_sync_indexes_binary_as_asset_and_skips_oversized_text() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "bin.dat", &[0u8, 1, 2, 3, 0, 5]);
        let big = vec![b'a'; (MAX_DOC_BYTES + 1) as usize];
        write(root, "big.txt", &big);
        write(root, "ok.txt", b"hello");
        let reg = Registry::with_default_languages();
        let stats = sync_docs(&mut s, root, &reg).unwrap();
        assert_eq!(stats.added, 2, "text + binary asset");
        assert_eq!(stats.skipped, 1, "oversized text is skipped");
        let kinds: Vec<(String, String)> = s
            .connection()
            .prepare("SELECT path, kind FROM docs ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(kinds, vec![("bin.dat".into(), "binary".into()), ("ok.txt".into(), "text".into())]);
        let assets_rows: i64 = s.connection().query_row("SELECT COUNT(*) FROM assets", [], |r| r.get(0)).unwrap();
        assert_eq!(assets_rows, 1);
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn test_docs_asset_image_searchable_by_name_and_description_survives_reindex() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "design/hero-banner.png", &png(1200, 400));
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let rec = describe(&s, "design/hero-banner.png").unwrap().expect("asset record");
        assert_eq!(rec.kind, "image");
        assert_eq!((rec.width, rec.height), (Some(1200), Some(400)));
        assert!(rec.description.is_none());
        assert!(rec.body.starts_with("[image 1200x400 image/png]"));

        // A model views the image once and stores what it saw.
        assert!(set_description(&mut s, "design/hero-banner.png", Some("Landing page hero: blue gradient, product logo centred"), Some("claude")).unwrap());
        let r = search_docs(&s, "gradient logo", &SearchOptions::default()).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].kind, "image");
        assert!(r.files[0].hits[0].text.contains("description:"));

        // The bytes change (re-export at a new size, with a trailing chunk so
        // the byte length differs — same-length rewrites within one second
        // are indistinguishable to the stat fast path by design) →
        // re-indexed, description kept.
        let mut bigger = png(2400, 800);
        bigger.extend_from_slice(b"IEND-extra-bytes");
        write(root, "design/hero-banner.png", &bigger);
        let stats = sync_docs(&mut s, root, &reg).unwrap();
        assert_eq!(stats.replaced, 1);
        let rec = describe(&s, "design/hero-banner.png").unwrap().unwrap();
        assert_eq!(rec.width, Some(2400));
        assert_eq!(rec.description.as_deref(), Some("Landing page hero: blue gradient, product logo centred"));
        assert_eq!(rec.described_by.as_deref(), Some("claude"));
        assert!(rec.body.contains("description: Landing page hero"));

        // Clearing removes it from search.
        assert!(set_description(&mut s, "design/hero-banner.png", None, None).unwrap());
        let r = search_docs(&s, "gradient", &SearchOptions::default()).unwrap();
        assert!(r.files.is_empty());
        // Not an asset → false.
        write(root, "notes.txt", b"plain");
        sync_docs(&mut s, root, &reg).unwrap();
        assert!(!set_description(&mut s, "notes.txt", Some("x"), None).unwrap());
        assert!(describe(&s, "notes.txt").unwrap().is_none());
    }

    #[test]
    fn test_docs_asset_kind_filter_in_search() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.png", &png(2, 2));
        write(root, "a.txt", b"image banner text");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        set_description(&mut s, "a.png", Some("banner"), None).unwrap();
        let imgs = search_docs(&s, "banner", &SearchOptions { kind: Some("image".into()), ..Default::default() }).unwrap();
        assert_eq!(imgs.files.len(), 1);
        assert_eq!(imgs.files[0].path, "a.png");
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
    fn test_search_relaxes_to_or_when_no_file_has_all_terms() {
        let (dir, mut s) = project();
        let root = dir.path();
        write(root, "a.txt", b"retry with exponential backoff\n");
        write(root, "b.txt", b"roadmap for the quarter\n");
        let reg = Registry::with_default_languages();
        sync_docs(&mut s, root, &reg).unwrap();
        let r = search_docs(&s, "backoff roadmap", &SearchOptions::default()).unwrap();
        assert!(r.relaxed, "no file has both terms → OR pass");
        assert_eq!(r.files.len(), 2);
        // A single-term miss stays a miss; explicit operators are never relaxed.
        assert!(!search_docs(&s, "zzz", &SearchOptions::default()).unwrap().relaxed);
        let strict = search_docs(&s, "backoff AND roadmap", &SearchOptions::default()).unwrap();
        assert!(!strict.relaxed && strict.files.is_empty());
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
