//! Heuristic resolution phases — run after the exact SQL phases in
//! [`crate::storage::resolve`] and only over rows those phases left NULL.
//!
//! The exact phases link a reference only when the raw text equals a
//! symbol's qualified name, or equals a bare symbol name in the *same*
//! file. Real code almost never calls things that way: Python writes
//! `from pkg.mod import helper; helper()`, TypeScript writes
//! `import { helper } from "./mod"; helper()`, Rust writes
//! `use crate::util::helper; helper()`, and every OO language writes
//! `self.method()` / `this.method()`. All of those left `callee_symbol_id`
//! NULL in v1, so `lens refs`, `lens follow`'s caller list, `lens map`'s
//! hot-spot ranking and `lens query`'s traversal silently missed most
//! cross-file edges. This module closes that gap with four rules, in
//! precedence order:
//!
//! 1. **Import → file.** Each `imports` row is mapped to a target file (or
//!    directory, for Go packages) using the importing file's language:
//!    module paths for Python (`pkg.mod` → `pkg/mod.py`, relative `.mod`)
//!    and Rust (`crate::a::b` → `<crate root>/a/b.rs`, `super::`, `self::`),
//!    relative paths for TypeScript / JavaScript (`./x` → `x.ts` /
//!    `x/index.ts` …), and package directories for Go. When the import
//!    names a symbol in that file, `resolved_symbol_id` is filled too.
//! 2. **Receiver strip.** `self.x` / `this.x` / `Self::x` / `self::x` /
//!    `super.x` resolve `x` against the same file (innermost definition
//!    wins by lowest id, which is declaration order).
//! 3. **Import-aware bare and dotted names.** A bare `helper` resolves via
//!    an import of the same file whose alias is `helper`, whose resolved
//!    symbol is named `helper`, or whose resolved file/directory defines a
//!    `helper`. A dotted `mod.helper` resolves when `mod` is an import
//!    alias or the last segment of an import path, else against the same
//!    file (an `obj.method()` call where `obj`'s class is local).
//! 4. **Unique name.** A bare name defined exactly once in the whole
//!    project resolves to that definition. Dotted names are *not* eligible
//!    — `list.append` must never link to an unrelated project `append`.
//!
//! Everything here is a heuristic and is labelled as such in the module
//! docs; the exact phases always win because they run first and this pass
//! touches only NULL rows. Wrong links are cheap to spot (`lens refs`
//! shows the site) and cheaper than missing edges, which are invisible.

use std::collections::HashMap;

use rusqlite::{params, Transaction};

use crate::error::{LensError, Result};
use crate::lang::{LanguageId, Registry};

/// Counts of FK fills performed by [`run`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HeuristicStats {
    pub imports_to_file: u64,
    pub imports_to_symbol: u64,
    pub calls: u64,
    pub refs: u64,
    pub types: u64,
}

#[derive(Debug, Clone)]
struct FileRow {
    id: i64,
    path: String,
    language: Option<LanguageId>,
    module_path: String,
}

#[derive(Debug, Clone)]
enum Target {
    File(i64),
    /// Go package: any file under this directory (project-relative, `/`).
    Dir(String),
}

#[derive(Debug, Clone)]
struct ImportRow {
    id: i64,
    file_id: i64,
    raw_path: String,
    alias: Option<String>,
    resolved_symbol_id: Option<i64>,
    resolved_file_id: Option<i64>,
    /// Computed target for this pass (persisted when it is a file).
    target: Option<Target>,
}

struct SymbolIndex {
    /// name → [(symbol_id, file_id)] in id order.
    by_name: HashMap<String, Vec<(i64, i64)>>,
    /// symbol_id → file_id.
    file_of: HashMap<i64, i64>,
    /// symbol_id → name.
    name_of: HashMap<i64, String>,
}

impl SymbolIndex {
    fn in_file(&self, name: &str, file_id: i64) -> Option<i64> {
        self.by_name
            .get(name)?
            .iter()
            .find(|(_, f)| *f == file_id)
            .map(|(s, _)| *s)
    }

    fn in_dir(&self, name: &str, dir: &str, files: &HashMap<i64, FileRow>) -> Option<i64> {
        self.by_name.get(name)?.iter().find_map(|(sid, fid)| {
            let f = files.get(fid)?;
            if parent_dir(&f.path) == dir {
                Some(*sid)
            } else {
                None
            }
        })
    }

    fn unique(&self, name: &str) -> Option<i64> {
        match self.by_name.get(name) {
            Some(v) if v.len() == 1 => Some(v[0].0),
            _ => None,
        }
    }
}

/// Run all heuristic phases inside `tx`. Idempotent: only NULL rows are
/// examined, and a second run finds nothing left to fill.
pub fn run(tx: &Transaction<'_>) -> Result<HeuristicStats> {
    let mut stats = HeuristicStats::default();
    let registry = Registry::with_default_languages();

    // ---- Load files + module paths -----------------------------------
    let files: HashMap<i64, FileRow> = {
        let mut stmt = tx
            .prepare("SELECT id, path, language FROM files")
            .map_err(|e| LensError::other(format!("heuristics: prepare files: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| LensError::other(format!("heuristics: query files: {e}")))?;
        let mut map = HashMap::new();
        for r in rows {
            let (id, path, lang) = r.map_err(|e| LensError::other(format!("heuristics: row files: {e}")))?;
            let language = LanguageId::from_label(&lang);
            let module_path = match language.and_then(|l| registry.by_id(l)) {
                Some(ext) => ext.module_path_from_relative_path(&path),
                None => String::new(),
            };
            map.insert(id, FileRow { id, path, language, module_path });
        }
        map
    };
    if files.is_empty() {
        return Ok(stats);
    }
    let file_by_path: HashMap<&str, i64> = files.values().map(|f| (f.path.as_str(), f.id)).collect();
    let file_by_module: HashMap<&str, i64> = files
        .values()
        .filter(|f| !f.module_path.is_empty())
        .map(|f| (f.module_path.as_str(), f.id))
        .collect();

    // ---- Load symbols ------------------------------------------------
    let symbols: SymbolIndex = {
        let mut stmt = tx
            .prepare("SELECT id, file_id, name FROM symbols ORDER BY id ASC")
            .map_err(|e| LensError::other(format!("heuristics: prepare symbols: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| LensError::other(format!("heuristics: query symbols: {e}")))?;
        let mut by_name: HashMap<String, Vec<(i64, i64)>> = HashMap::new();
        let mut file_of = HashMap::new();
        let mut name_of = HashMap::new();
        for r in rows {
            let (id, fid, name) = r.map_err(|e| LensError::other(format!("heuristics: row symbols: {e}")))?;
            by_name.entry(name.clone()).or_default().push((id, fid));
            file_of.insert(id, fid);
            name_of.insert(id, name);
        }
        SymbolIndex { by_name, file_of, name_of }
    };

    // ---- Phase 1: imports → file / symbol -----------------------------
    let mut imports: Vec<ImportRow> = {
        let mut stmt = tx
            .prepare("SELECT id, file_id, raw_path, alias, resolved_symbol_id, resolved_file_id FROM imports ORDER BY id ASC")
            .map_err(|e| LensError::other(format!("heuristics: prepare imports: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ImportRow {
                    id: r.get(0)?,
                    file_id: r.get(1)?,
                    raw_path: r.get(2)?,
                    alias: r.get(3)?,
                    resolved_symbol_id: r.get(4)?,
                    resolved_file_id: r.get(5)?,
                    target: None,
                })
            })
            .map_err(|e| LensError::other(format!("heuristics: query imports: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LensError::other(format!("heuristics: collect imports: {e}")))?
    };

    let mut import_updates: Vec<(i64, Option<i64>, Option<i64>)> = Vec::new();
    for imp in imports.iter_mut() {
        if let Some(fid) = imp.resolved_file_id {
            imp.target = Some(Target::File(fid));
            continue;
        }
        let Some(importer) = files.get(&imp.file_id) else { continue };
        let target = resolve_import_target(importer, &imp.raw_path, &file_by_path, &file_by_module, &files);
        match target {
            Some(Target::File(fid)) => {
                // Does the import name a symbol in that file? Leaf = last
                // segment of raw_path (or the alias for default imports).
                let leaf = last_segment(&imp.raw_path);
                let sym = symbols.in_file(leaf, fid);
                let new_sym = imp.resolved_symbol_id.or(sym);
                import_updates.push((imp.id, new_sym, Some(fid)));
                if new_sym.is_some() && imp.resolved_symbol_id.is_none() {
                    stats.imports_to_symbol += 1;
                }
                stats.imports_to_file += 1;
                imp.resolved_file_id = Some(fid);
                imp.resolved_symbol_id = new_sym;
                imp.target = Some(Target::File(fid));
            }
            Some(Target::Dir(d)) => {
                imp.target = Some(Target::Dir(d));
            }
            None => {}
        }
    }
    if !import_updates.is_empty() {
        let mut upd = tx
            .prepare("UPDATE imports SET resolved_symbol_id = ?1, resolved_file_id = ?2 WHERE id = ?3")
            .map_err(|e| LensError::other(format!("heuristics: prepare update imports: {e}")))?;
        for (id, sym, fid) in &import_updates {
            upd.execute(params![sym, fid, id])
                .map_err(|e| LensError::other(format!("heuristics: update import {id}: {e}")))?;
        }
    }

    // Per-file import lookup for the name phases.
    let mut imports_by_file: HashMap<i64, Vec<&ImportRow>> = HashMap::new();
    for imp in &imports {
        imports_by_file.entry(imp.file_id).or_default().push(imp);
    }

    // ---- Phases 2–4 over calls / refs / types --------------------------
    let ctx = NameContext { files: &files, symbols: &symbols, imports_by_file: &imports_by_file };
    stats.calls = fill_table(tx, &ctx, "calls", "callee_symbol_id", "callee_raw_name")?;
    stats.refs = fill_table(tx, &ctx, "refs", "symbol_id", "raw_name")?;
    stats.types = fill_table(tx, &ctx, "types", "target_symbol_id", "target_raw_name")?;
    Ok(stats)
}

struct NameContext<'a> {
    files: &'a HashMap<i64, FileRow>,
    symbols: &'a SymbolIndex,
    imports_by_file: &'a HashMap<i64, Vec<&'a ImportRow>>,
}

/// Stream NULL rows of one table, resolve each raw name, batch the updates.
fn fill_table(tx: &Transaction<'_>, ctx: &NameContext<'_>, table: &str, fk: &str, raw_col: &str) -> Result<u64> {
    let updates: Vec<(i64, i64)> = {
        let sql = format!("SELECT id, file_id, {raw_col} FROM {table} WHERE {fk} IS NULL");
        let mut stmt = tx
            .prepare(&sql)
            .map_err(|e| LensError::other(format!("heuristics: prepare {table}: {e}")))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| LensError::other(format!("heuristics: query {table}: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let (id, file_id, raw) = r.map_err(|e| LensError::other(format!("heuristics: row {table}: {e}")))?;
            if let Some(sid) = resolve_name(ctx, file_id, &raw) {
                out.push((id, sid));
            }
        }
        out
    };
    if updates.is_empty() {
        return Ok(0);
    }
    let sql = format!("UPDATE {table} SET {fk} = ?1 WHERE id = ?2");
    let mut upd = tx
        .prepare(&sql)
        .map_err(|e| LensError::other(format!("heuristics: prepare update {table}: {e}")))?;
    for (id, sid) in &updates {
        upd.execute(params![sid, id])
            .map_err(|e| LensError::other(format!("heuristics: update {table} {id}: {e}")))?;
    }
    Ok(updates.len() as u64)
}

/// Resolve one raw name occurring in `file_id`. See module docs for rules.
fn resolve_name(ctx: &NameContext<'_>, file_id: i64, raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Rule 2 — receiver strip.
    for prefix in ["self.", "this.", "Self::", "self::", "super.", "super::", "cls."] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            let leaf = last_segment(rest);
            if is_ident(leaf) {
                return ctx.symbols.in_file(leaf, file_id);
            }
            return None;
        }
    }

    let (head, leaf) = split_head_leaf(raw);
    if !is_ident(leaf) {
        return None; // computed access, call on a call, etc.
    }

    let imports = ctx.imports_by_file.get(&file_id);
    match head {
        None => {
            // Rule 3 — bare name via imports.
            if let Some(imports) = imports {
                for imp in imports {
                    let alias_hit = imp.alias.as_deref() == Some(leaf);
                    let sym_hit = imp
                        .resolved_symbol_id
                        .and_then(|s| ctx.symbols.name_of.get(&s))
                        .is_some_and(|n| n == leaf);
                    let path_leaf_hit = last_segment(&imp.raw_path) == leaf;
                    if alias_hit || sym_hit || path_leaf_hit {
                        if let Some(sid) = imp.resolved_symbol_id {
                            if ctx.symbols.name_of.get(&sid).is_some_and(|n| n == leaf) {
                                return Some(sid);
                            }
                        }
                        if let Some(sid) = target_lookup(ctx, imp.target.as_ref(), leaf) {
                            return Some(sid);
                        }
                    }
                }
            }
            // Rule 4 — unique project-wide definition.
            ctx.symbols.unique(leaf)
        }
        Some(head) => {
            // Rule 3 — dotted name whose head is an import alias / module.
            if let Some(imports) = imports {
                for imp in imports {
                    let alias_hit = imp.alias.as_deref() == Some(head);
                    let path_hit = last_segment(&imp.raw_path) == head;
                    if alias_hit || path_hit {
                        if let Some(sid) = target_lookup(ctx, imp.target.as_ref(), leaf) {
                            return Some(sid);
                        }
                        // `import pkg.mod` then `pkg.mod.f()` — the import's
                        // resolved symbol may itself be `mod`; try its file.
                        if let Some(sid) = imp.resolved_symbol_id {
                            if let Some(fid) = ctx.symbols.file_of.get(&sid) {
                                if let Some(hit) = ctx.symbols.in_file(leaf, *fid) {
                                    return Some(hit);
                                }
                            }
                        }
                    }
                }
            }
            // Same-file `obj.method()` where the class is local.
            ctx.symbols.in_file(leaf, file_id)
        }
    }
}

fn target_lookup(ctx: &NameContext<'_>, target: Option<&Target>, leaf: &str) -> Option<i64> {
    match target? {
        Target::File(fid) => ctx.symbols.in_file(leaf, *fid),
        Target::Dir(dir) => ctx.symbols.in_dir(leaf, dir, ctx.files),
    }
}

/// Map an import's raw path to a project file or directory using the
/// importing file's language conventions. `None` for external packages.
fn resolve_import_target(
    importer: &FileRow,
    raw_path: &str,
    file_by_path: &HashMap<&str, i64>,
    file_by_module: &HashMap<&str, i64>,
    files: &HashMap<i64, FileRow>,
) -> Option<Target> {
    match importer.language? {
        LanguageId::Python => {
            let module = python_absolute_module(importer, raw_path)?;
            module_or_parent(&module, '.', file_by_module)
        }
        LanguageId::Rust => {
            let module = rust_absolute_module(importer, raw_path)?;
            module_or_parent(&module, ':', file_by_module)
        }
        LanguageId::TypeScript | LanguageId::JavaScript => {
            if !(raw_path.starts_with("./") || raw_path.starts_with("../") || raw_path == "." || raw_path == "..") {
                return None;
            }
            let base = join_relative(&parent_dir(&importer.path), raw_path);
            const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "d.ts"];
            if let Some(id) = file_by_path.get(base.as_str()) {
                return Some(Target::File(*id));
            }
            for ext in EXTS {
                if let Some(id) = file_by_path.get(format!("{base}.{ext}").as_str()) {
                    return Some(Target::File(*id));
                }
            }
            for ext in EXTS {
                if let Some(id) = file_by_path.get(format!("{base}/index.{ext}").as_str()) {
                    return Some(Target::File(*id));
                }
            }
            None
        }
        LanguageId::Go => {
            // Package import: match a project directory whose tail equals
            // the import path's tail (e.g. `example.com/app/internal/store`
            // → `internal/store`). Longest matching suffix wins.
            let raw = raw_path.trim_matches('"');
            let segs: Vec<&str> = raw.split('/').filter(|s| !s.is_empty()).collect();
            if segs.is_empty() {
                return None;
            }
            let mut best: Option<(usize, String)> = None;
            for f in files.values() {
                if f.language != Some(LanguageId::Go) {
                    continue;
                }
                let dir = parent_dir(&f.path);
                let dsegs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
                let mut n = 0;
                while n < segs.len() && n < dsegs.len() && segs[segs.len() - 1 - n] == dsegs[dsegs.len() - 1 - n] {
                    n += 1;
                }
                if n > 0 && best.as_ref().is_none_or(|(bn, _)| n > *bn) {
                    best = Some((n, dir.clone()));
                }
            }
            best.map(|(_, d)| Target::Dir(d))
        }
        _ => None,
    }
}

/// Try `module` as a file's module path, else its parent (the import named
/// a symbol inside the parent module).
fn module_or_parent(module: &str, sep: char, file_by_module: &HashMap<&str, i64>) -> Option<Target> {
    if let Some(id) = file_by_module.get(module) {
        return Some(Target::File(*id));
    }
    let sep_str = if sep == ':' { "::" } else { "." };
    let parent = module.rsplit_once(sep_str)?.0;
    file_by_module.get(parent).map(|id| Target::File(*id))
}

/// Python: absolute dotted module for `raw_path` as seen from `importer`.
/// Relative imports (`.mod`, `..pkg.mod`) resolve against the importer's
/// package. Returns `None` for an empty result.
fn python_absolute_module(importer: &FileRow, raw_path: &str) -> Option<String> {
    let dots = raw_path.chars().take_while(|c| *c == '.').count();
    if dots == 0 {
        return Some(raw_path.to_string());
    }
    let rest = &raw_path[dots..];
    // Importer package: module path minus its own leaf (unless it is a
    // package `__init__`, whose module path already is the package).
    let mut pkg: Vec<&str> = importer.module_path.split('.').filter(|s| !s.is_empty()).collect();
    if !importer.path.ends_with("__init__.py") {
        pkg.pop();
    }
    for _ in 1..dots {
        pkg.pop()?;
    }
    let mut out = pkg.join(".");
    if !rest.is_empty() {
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(rest);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Rust: rewrite `crate::` / `self::` / `super::` prefixes into the module
/// path space used by [`crate::lang::LanguageExtractor::module_path_from_relative_path`].
fn rust_absolute_module(importer: &FileRow, raw_path: &str) -> Option<String> {
    let m = &importer.module_path;
    if let Some(rest) = raw_path.strip_prefix("crate::") {
        let root = rust_crate_root(m);
        return Some(if root.is_empty() { rest.to_string() } else { format!("{root}::{rest}") });
    }
    if let Some(rest) = raw_path.strip_prefix("self::") {
        return Some(if m.is_empty() { rest.to_string() } else { format!("{m}::{rest}") });
    }
    if let Some(rest) = raw_path.strip_prefix("super::") {
        let parent = m.rsplit_once("::").map(|(p, _)| p).unwrap_or("");
        return Some(if parent.is_empty() { rest.to_string() } else { format!("{parent}::{rest}") });
    }
    // `std::…`, external crates, or an already-absolute project path.
    Some(raw_path.to_string())
}

/// The module-path prefix up to and including the first `src` segment
/// (`crates::lens-core::src::storage::db` → `crates::lens-core::src`).
/// Falls back to the empty prefix for files outside a `src/` tree.
fn rust_crate_root(module_path: &str) -> String {
    let segs: Vec<&str> = module_path.split("::").collect();
    match segs.iter().position(|s| *s == "src") {
        Some(i) => segs[..=i].join("::"),
        None => String::new(),
    }
}

fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Join `rel` (`./a/../b`) onto `dir` and normalise `.` / `..`.
fn join_relative(dir: &str, rel: &str) -> String {
    let mut segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    segs.join("/")
}

/// Split `a.b.c` / `a::b::c` into `(Some("a"), "c")`; bare names give
/// `(None, name)`. Call suffixes like `foo()` are not expected here (the
/// extractors record the callee text only) but parentheses are stripped
/// defensively.
fn split_head_leaf(raw: &str) -> (Option<&str>, &str) {
    let raw = raw.trim_end_matches("()");
    let idx = raw.rfind("::").map(|i| (i, 2)).into_iter().chain(raw.rfind('.').map(|i| (i, 1))).max();
    match idx {
        Some((i, w)) => {
            let head_full = &raw[..i];
            let head = head_full.split(['.', ':']).next().unwrap_or(head_full);
            (Some(head), &raw[i + w..])
        }
        None => (None, raw),
    }
}

fn last_segment(s: &str) -> &str {
    s.rsplit(['.', ':', '/']).next().unwrap_or(s)
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ExtractedCall, ExtractedFile, ExtractedImport, ExtractedSymbol};
    use crate::storage::insert::insert_extracted_files;
    use crate::storage::resolve::resolve_cross_file_references;
    use crate::storage::Storage;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        (dir, Storage::open(&path).unwrap())
    }

    fn sym(qname: &str, name: &str) -> ExtractedSymbol {
        ExtractedSymbol {
            qualified_name: qname.into(),
            name: name.into(),
            kind: "function".into(),
            start_line: 1,
            start_col: 0,
            end_line: 3,
            end_col: 0,
            body_start_byte: 0,
            body_end_byte: 0,
            signature: None,
            visibility: None,
            parent_qualified_name: None,
            doc_comment: None,
        }
    }

    fn file(path: &str, lang: LanguageId, syms: Vec<ExtractedSymbol>) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, lang);
        ef.content_hash = [1u8; 32];
        ef.size_bytes = 10;
        ef.modified_at = 1;
        ef.symbols = syms;
        ef
    }

    fn call(caller: &str, callee: &str) -> ExtractedCall {
        ExtractedCall { caller_qualified_name: caller.into(), callee_raw_name: callee.into(), line: 2, col: 0 }
    }

    fn import(raw: &str, alias: Option<&str>) -> ExtractedImport {
        ExtractedImport { raw_path: raw.into(), alias: alias.map(str::to_string), line: 1 }
    }

    fn callee_of(s: &Storage, caller_qname: &str) -> Option<String> {
        s.connection()
            .query_row(
                "SELECT t.qualified_name FROM calls c
                 JOIN symbols c1 ON c1.id = c.caller_symbol_id
                 LEFT JOIN symbols t ON t.id = c.callee_symbol_id
                 WHERE c1.qualified_name = ?1",
                params![caller_qname],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
    }

    fn import_file_of(s: &Storage, raw: &str) -> Option<String> {
        s.connection()
            .query_row(
                "SELECT f.path FROM imports i LEFT JOIN files f ON f.id = i.resolved_file_id WHERE i.raw_path = ?1",
                params![raw],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
    }

    #[test]
    fn test_heuristics_python_from_import_links_cross_file_call() {
        let (_g, mut s) = tmp_storage();
        let lib = file("pkg/util.py", LanguageId::Python, vec![sym("pkg.util.helper", "helper")]);
        let mut app = file("pkg/app.py", LanguageId::Python, vec![sym("pkg.app.main", "main")]);
        app.imports.push(import("pkg.util.helper", None)); // from pkg.util import helper
        app.calls.push(call("pkg.app.main", "helper"));
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "pkg.app.main").as_deref(), Some("pkg.util.helper"));
        assert_eq!(import_file_of(&s, "pkg.util.helper").as_deref(), Some("pkg/util.py"));
    }

    #[test]
    fn test_heuristics_python_module_import_dotted_call() {
        let (_g, mut s) = tmp_storage();
        let lib = file("pkg/util.py", LanguageId::Python, vec![sym("pkg.util.helper", "helper")]);
        let mut app = file("pkg/app.py", LanguageId::Python, vec![sym("pkg.app.main", "main")]);
        app.imports.push(import("pkg.util", Some("u"))); // import pkg.util as u
        app.calls.push(call("pkg.app.main", "u.helper"));
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "pkg.app.main").as_deref(), Some("pkg.util.helper"));
    }

    #[test]
    fn test_heuristics_python_relative_import_resolves_against_package() {
        let (_g, mut s) = tmp_storage();
        let lib = file("pkg/sub/util.py", LanguageId::Python, vec![sym("pkg.sub.util.helper", "helper")]);
        let mut app = file("pkg/sub/app.py", LanguageId::Python, vec![sym("pkg.sub.app.main", "main")]);
        app.imports.push(import(".util.helper", None)); // from .util import helper
        app.calls.push(call("pkg.sub.app.main", "helper"));
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "pkg.sub.app.main").as_deref(), Some("pkg.sub.util.helper"));
    }

    #[test]
    fn test_heuristics_rust_crate_path_use_links_call() {
        let (_g, mut s) = tmp_storage();
        let lib = file("src/util.rs", LanguageId::Rust, vec![sym("src::util::helper", "helper")]);
        let mut main = file("src/main.rs", LanguageId::Rust, vec![sym("src::main", "main")]);
        main.imports.push(import("crate::util::helper", None));
        main.calls.push(call("src::main", "helper"));
        insert_extracted_files(&mut s, &[lib, main]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "src::main").as_deref(), Some("src::util::helper"));
        assert_eq!(import_file_of(&s, "crate::util::helper").as_deref(), Some("src/util.rs"));
    }

    #[test]
    fn test_heuristics_rust_workspace_crate_root_detected() {
        let (_g, mut s) = tmp_storage();
        let lib = file(
            "crates/core/src/error.rs",
            LanguageId::Rust,
            vec![sym("crates::core::src::error::LensError", "LensError")],
        );
        let mut db = file("crates/core/src/db.rs", LanguageId::Rust, vec![sym("crates::core::src::db::open", "open")]);
        db.imports.push(import("crate::error::LensError", None));
        db.calls.push(call("crates::core::src::db::open", "LensError::other"));
        insert_extracted_files(&mut s, &[lib, db]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(import_file_of(&s, "crate::error::LensError").as_deref(), Some("crates/core/src/error.rs"));
    }

    #[test]
    fn test_heuristics_typescript_relative_import_with_index_and_ext() {
        let (_g, mut s) = tmp_storage();
        let lib = file("src/lib/index.ts", LanguageId::TypeScript, vec![sym("src::lib::index::helper", "helper")]);
        let mut app = file("src/app.ts", LanguageId::TypeScript, vec![sym("src::app::main", "main")]);
        app.imports.push(import("./lib", Some("helper"))); // import { helper } from "./lib"
        app.calls.push(call("src::app::main", "helper"));
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "src::app::main").as_deref(), Some("src::lib::index::helper"));
        assert_eq!(import_file_of(&s, "./lib").as_deref(), Some("src/lib/index.ts"));
    }

    #[test]
    fn test_heuristics_typescript_external_package_stays_unresolved() {
        let (_g, mut s) = tmp_storage();
        let mut app = file("src/app.ts", LanguageId::TypeScript, vec![sym("src::app::main", "main")]);
        app.imports.push(import("react", Some("useState")));
        app.calls.push(call("src::app::main", "useState"));
        insert_extracted_files(&mut s, &[app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "src::app::main"), None);
        assert_eq!(import_file_of(&s, "react"), None);
    }

    #[test]
    fn test_heuristics_go_package_import_dotted_call() {
        let (_g, mut s) = tmp_storage();
        let store = file("internal/store/store.go", LanguageId::Go, vec![sym("internal::store::Open", "Open")]);
        let mut main = file("cmd/app/main.go", LanguageId::Go, vec![sym("cmd::app::main", "main")]);
        main.imports.push(import("example.com/app/internal/store", None));
        main.calls.push(call("cmd::app::main", "store.Open"));
        insert_extracted_files(&mut s, &[store, main]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "cmd::app::main").as_deref(), Some("internal::store::Open"));
    }

    #[test]
    fn test_heuristics_self_and_this_receiver_resolve_same_file() {
        let (_g, mut s) = tmp_storage();
        let mut py = file(
            "svc.py",
            LanguageId::Python,
            vec![sym("svc.Svc", "Svc"), sym("svc.Svc.run", "run"), sym("svc.Svc.step", "step")],
        );
        py.calls.push(call("svc.Svc.run", "self.step"));
        let mut ts = file("a.ts", LanguageId::TypeScript, vec![sym("a::A::go", "go"), sym("a::A::next", "next")]);
        ts.calls.push(call("a::A::go", "this.next"));
        insert_extracted_files(&mut s, &[py, ts]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "svc.Svc.run").as_deref(), Some("svc.Svc.step"));
        assert_eq!(callee_of(&s, "a::A::go").as_deref(), Some("a::A::next"));
    }

    #[test]
    fn test_heuristics_unique_bare_name_resolves_but_dotted_does_not() {
        let (_g, mut s) = tmp_storage();
        let lib = file("lib.py", LanguageId::Python, vec![sym("lib.only_once", "only_once"), sym("lib.append", "append")]);
        let mut app = file("app.py", LanguageId::Python, vec![sym("app.main", "main"), sym("app.other", "other")]);
        app.calls.push(call("app.main", "only_once")); // no import at all
        app.calls.push(call("app.other", "items.append")); // stdlib method — must stay NULL
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "app.main").as_deref(), Some("lib.only_once"));
        assert_eq!(callee_of(&s, "app.other"), None);
    }

    #[test]
    fn test_heuristics_ambiguous_bare_name_stays_unresolved() {
        let (_g, mut s) = tmp_storage();
        let a = file("a.py", LanguageId::Python, vec![sym("a.dup", "dup")]);
        let b = file("b.py", LanguageId::Python, vec![sym("b.dup", "dup")]);
        let mut app = file("app.py", LanguageId::Python, vec![sym("app.main", "main")]);
        app.calls.push(call("app.main", "dup"));
        insert_extracted_files(&mut s, &[a, b, app]).unwrap();
        resolve_cross_file_references(&mut s).unwrap();
        assert_eq!(callee_of(&s, "app.main"), None, "two candidates, no import evidence → leave NULL");
    }

    #[test]
    fn test_heuristics_is_idempotent() {
        let (_g, mut s) = tmp_storage();
        let lib = file("pkg/util.py", LanguageId::Python, vec![sym("pkg.util.helper", "helper")]);
        let mut app = file("pkg/app.py", LanguageId::Python, vec![sym("pkg.app.main", "main")]);
        app.imports.push(import("pkg.util.helper", None));
        app.calls.push(call("pkg.app.main", "helper"));
        insert_extracted_files(&mut s, &[lib, app]).unwrap();
        let first = resolve_cross_file_references(&mut s).unwrap();
        let second = resolve_cross_file_references(&mut s).unwrap();
        assert!(first.resolved_calls >= 1);
        assert_eq!(second.resolved_calls, 0);
        assert_eq!(second.resolved_imports, 0);
    }

    #[test]
    fn test_helpers_split_and_join() {
        assert_eq!(split_head_leaf("a.b.c"), (Some("a"), "c"));
        assert_eq!(split_head_leaf("a::b"), (Some("a"), "b"));
        assert_eq!(split_head_leaf("plain"), (None, "plain"));
        assert_eq!(join_relative("src/app", "../lib/x"), "src/lib/x");
        assert_eq!(join_relative("", "./a"), "a");
        assert_eq!(rust_crate_root("crates::core::src::db"), "crates::core::src");
        assert_eq!(rust_crate_root("main"), "");
        assert!(is_ident("_x1"));
        assert!(!is_ident("x[0]"));
    }
}
