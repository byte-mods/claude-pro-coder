use crate::lang::LanguageId;

/// Per-file extraction context handed to a [`crate::lang::LanguageExtractor`].
///
/// `module_path` is the project-relative module prefix used to qualify
/// top-level symbol names. The pipeline (T7) computes it from
/// [`crate::DiscoveredFile::relative_path`] using language-specific rules.
pub struct ExtractContext<'a> {
    pub relative_path: &'a str,
    pub module_path: &'a str,
}

#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub relative_path: String,
    pub language: LanguageId,
    pub symbols: Vec<ExtractedSymbol>,
    pub refs: Vec<ExtractedRef>,
    pub calls: Vec<ExtractedCall>,
    pub imports: Vec<ExtractedImport>,
    pub type_relations: Vec<ExtractedTypeRel>,
    /// blake3 hash of file contents — 32 bytes, matching `files.content_hash`
    /// in the SQLite schema. Populated by [`crate::extract::pipeline::run`]
    /// from [`crate::DiscoveredFile::content_hash`]. Zero-init when constructed
    /// via [`ExtractedFile::empty`] so per-language extractors do not need to
    /// know about file metadata.
    pub content_hash: [u8; 32],
    /// File size in bytes. Populated by the pipeline. Zero in [`empty`].
    pub size_bytes: u64,
    /// Last-modified Unix epoch seconds. Populated by the pipeline. Zero in [`empty`].
    pub modified_at: i64,
}

impl ExtractedFile {
    pub fn empty(relative_path: impl Into<String>, language: LanguageId) -> Self {
        Self {
            relative_path: relative_path.into(),
            language,
            symbols: Vec::new(),
            refs: Vec::new(),
            calls: Vec::new(),
            imports: Vec::new(),
            type_relations: Vec::new(),
            content_hash: [0u8; 32],
            size_bytes: 0,
            modified_at: 0,
        }
    }
}

/// One symbol — function, struct, enum, trait, impl method, etc.
/// Field shapes mirror the `symbols` table in the SQLite schema.
/// Lines are 1-indexed; columns and bytes are 0-indexed.
///
/// **u32 width caps (v1).** `start_line`, `end_line`, `start_col`, `end_col`,
/// `body_start_byte`, and `body_end_byte` are all `u32`. This caps a single
/// file at ~4.29 billion bytes / lines. For source files this is comfortably
/// over-budget; binary blobs or generated megalines are out of scope for v1.
/// Truncation is silent at the cast boundary in extractors — a future
/// version may switch to `u64` if needed.
///
/// **`body_start_byte` / `body_end_byte` semantics.** Despite the name,
/// these span the **whole declaration** node (e.g. for `fn foo() {...}` they
/// cover the entire `function_item`), not just the body block. The naming
/// is retained for SQLite-schema column compatibility. A future task may
/// rename these to `span_start_byte` / `span_end_byte` (column rename =
/// schema migration).
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub body_start_byte: u32,
    pub body_end_byte: u32,
    pub signature: Option<String>,
    pub visibility: Option<String>,
    /// Resolved to `parent_symbol_id` at insert time by the storage layer
    /// (Section 4). `None` for top-level symbols with no enclosing scope.
    pub parent_qualified_name: Option<String>,
    /// Schema v2: the doc comment immediately preceding (or, for Python,
    /// inside) the declaration. Per-language extractors populate this; an
    /// empty value should be stored as `None`, not as `Some("")`. The string
    /// is normalised: outer comment markers stripped, leading whitespace
    /// trimmed per line, lines re-joined with `\n`.
    pub doc_comment: Option<String>,
}

/// One identifier reference (T5 will populate). Mirrors the `refs` table.
#[derive(Debug, Clone)]
pub struct ExtractedRef {
    pub raw_name: String,
    pub kind: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// One call-site (T5 will populate). Mirrors the `calls` table.
#[derive(Debug, Clone)]
pub struct ExtractedCall {
    pub caller_qualified_name: String,
    pub callee_raw_name: String,
    pub line: u32,
    pub col: u32,
}

/// One import (T5 will populate). Mirrors the `imports` table.
#[derive(Debug, Clone)]
pub struct ExtractedImport {
    pub raw_path: String,
    pub alias: Option<String>,
    pub line: u32,
}

/// One type-system relation (T5 will populate). Mirrors the `types` table.
#[derive(Debug, Clone)]
pub struct ExtractedTypeRel {
    pub symbol_qualified_name: String,
    pub relation: String,
    pub target_raw_name: String,
    pub line: u32,
}
