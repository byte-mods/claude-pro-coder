//! Language registry and per-language extractors.
//!
//! Each supported language is identified by a stable [`LanguageId`] and
//! implemented by a type implementing [`LanguageExtractor`]. The [`Registry`]
//! resolves an extension or `LanguageId` to the matching extractor.

pub mod dart;
pub mod go;
pub mod javascript;
pub mod python;
pub mod registry;
pub mod rust;
pub mod typescript;

use std::fmt;

pub use dart::DartExtractor;
pub use go::GoExtractor;
pub use javascript::JavaScriptExtractor;
pub use python::PythonExtractor;
pub use registry::Registry;
pub use rust::RustExtractor;
pub use typescript::TypeScriptExtractor;

/// Stable language identifier — also used as the value of `files.language` in
/// the SQLite schema. New languages append a variant; existing names are
/// frozen as part of the schema contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Dart,
}

impl LanguageId {
    pub fn as_str(self) -> &'static str {
        match self {
            LanguageId::Rust => "rust",
            LanguageId::Python => "python",
            LanguageId::TypeScript => "typescript",
            LanguageId::JavaScript => "javascript",
            LanguageId::Go => "go",
            LanguageId::Dart => "dart",
        }
    }

    pub fn all() -> &'static [LanguageId] {
        &[
            LanguageId::Rust,
            LanguageId::Python,
            LanguageId::TypeScript,
            LanguageId::JavaScript,
            LanguageId::Go,
            LanguageId::Dart,
        ]
    }

    pub fn from_label(label: &str) -> Option<LanguageId> {
        match label {
            "rust" => Some(LanguageId::Rust),
            "python" => Some(LanguageId::Python),
            "typescript" => Some(LanguageId::TypeScript),
            "javascript" => Some(LanguageId::JavaScript),
            "go" => Some(LanguageId::Go),
            "dart" => Some(LanguageId::Dart),
            _ => None,
        }
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Static surface for a language. Per-language impls live in `lang::rust`,
/// `lang::python`, and so on. The default `extract` impl returns an empty
/// [`crate::extract::ExtractedFile`] so that test stubs and not-yet-wired
/// languages compile without manually implementing the method.
pub trait LanguageExtractor: Send + Sync + 'static {
    fn language_id(&self) -> LanguageId;
    fn extensions(&self) -> &'static [&'static str];
    fn tree_sitter_language(&self) -> tree_sitter::Language;
    fn extract(
        &self,
        _parsed: &crate::parse::ParsedFile,
        ctx: &crate::extract::ExtractContext,
    ) -> crate::extract::ExtractedFile {
        crate::extract::ExtractedFile::empty(ctx.relative_path, self.language_id())
    }
    /// Compute the module-path prefix used to qualify symbols extracted from
    /// `relative_path`. The pipeline calls this once per file before invoking
    /// [`Self::extract`] and threads the result through [`crate::extract::ExtractContext`].
    ///
    /// The default implementation strips the file extension from the final
    /// segment and replaces path separators with `::` (Rust-style). Languages
    /// with other conventions override:
    ///   - Python uses `.` as the segment separator and collapses `__init__.py`
    ///     to its parent directory.
    ///   - Rust collapses `lib.rs` / `main.rs` / `mod.rs` to their parent
    ///     directory.
    ///
    /// `relative_path` is project-relative with forward-slash separators
    /// (the contract of [`crate::DiscoveredFile::relative_path`]).
    fn module_path_from_relative_path(&self, relative_path: &str) -> String {
        let (parent, last) = match relative_path.rfind('/') {
            Some(i) => (&relative_path[..i], &relative_path[i + 1..]),
            None => ("", relative_path),
        };
        let parent_joined = parent.replace('/', "::");
        let stem = match last.rfind('.') {
            Some(i) => &last[..i],
            None => last,
        };
        if parent_joined.is_empty() {
            stem.to_string()
        } else if stem.is_empty() {
            parent_joined
        } else {
            format!("{parent_joined}::{stem}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_id_as_str_round_trips_via_from_label() {
        for lang in LanguageId::all() {
            let s = lang.as_str();
            assert_eq!(LanguageId::from_label(s), Some(*lang));
        }
    }

    #[test]
    fn test_language_id_from_label_returns_none_for_unknown() {
        assert!(LanguageId::from_label("cobol").is_none());
        assert!(LanguageId::from_label("").is_none());
    }

    #[test]
    fn test_language_id_display_matches_as_str() {
        assert_eq!(format!("{}", LanguageId::Rust), "rust");
        assert_eq!(format!("{}", LanguageId::Python), "python");
    }

    /// A LanguageExtractor that does not override `module_path_from_relative_path`,
    /// used to test the default impl directly.
    struct DefaultStub;

    impl LanguageExtractor for DefaultStub {
        fn language_id(&self) -> LanguageId {
            LanguageId::Rust
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["rs"]
        }
        fn tree_sitter_language(&self) -> tree_sitter::Language {
            tree_sitter_rust::language()
        }
    }

    #[test]
    fn test_default_module_path_strips_extension_and_uses_double_colon_separator() {
        let s = DefaultStub;
        assert_eq!(s.module_path_from_relative_path("src/foo.rs"), "src::foo");
        assert_eq!(s.module_path_from_relative_path("foo.rs"), "foo");
    }

    #[test]
    fn test_default_module_path_handles_empty_string() {
        let s = DefaultStub;
        assert_eq!(s.module_path_from_relative_path(""), "");
    }

    #[test]
    fn test_default_module_path_for_path_without_extension_keeps_it_verbatim() {
        let s = DefaultStub;
        assert_eq!(s.module_path_from_relative_path("src/Makefile"), "src::Makefile");
    }

    #[test]
    fn test_default_module_path_for_deep_path() {
        let s = DefaultStub;
        assert_eq!(
            s.module_path_from_relative_path("a/b/c/d.rs"),
            "a::b::c::d"
        );
    }
}
