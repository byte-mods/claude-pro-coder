//! Extension/id → [`LanguageExtractor`] lookup.
//!
//! The default registry is intentionally empty; per-language modules
//! (`lang::rust`, `lang::python`) wire themselves in by replacing the
//! [`Registry::default`] body in later tasks. The empty default keeps T1
//! independent of the per-language extractors that ship in T4 and T6.

use std::collections::HashMap;
use std::sync::Arc;

use super::{LanguageExtractor, LanguageId};

#[derive(Clone, Default)]
pub struct Registry {
    by_ext: HashMap<&'static str, Arc<dyn LanguageExtractor>>,
    by_id: HashMap<LanguageId, Arc<dyn LanguageExtractor>>,
}

impl Registry {
    pub fn empty() -> Self {
        Self {
            by_ext: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub fn with_default_languages() -> Self {
        // Per-language extractors are registered here as they land.
        //   T4 (Section 2 part 1) — Rust          ✓
        //   T6 (Section 2 part 2) — Python        ✓
        //   30%-coverage    — TypeScript (.ts)    ✓
        //   30%-coverage    — TypeScript (.tsx)   ✓
        //   30%-coverage    — JavaScript (.js)    ✓
        //   30%-coverage    — JavaScript (.jsx)   ✓
        //   30%-coverage    — Go         (.go)    ✓
        //   30%-coverage    — Dart       (.dart)  ✓
        let mut r = Self::empty();
        r.register(Arc::new(super::rust::RustExtractor));
        r.register(Arc::new(super::python::PythonExtractor));
        r.register(Arc::new(super::typescript::TypeScriptExtractor::ts()));
        r.register(Arc::new(super::typescript::TypeScriptExtractor::tsx()));
        r.register(Arc::new(super::javascript::JavaScriptExtractor::js()));
        r.register(Arc::new(super::javascript::JavaScriptExtractor::jsx()));
        r.register(Arc::new(super::go::GoExtractor::new()));
        r.register(Arc::new(super::dart::DartExtractor::new()));
        r
    }

    pub fn register(&mut self, extractor: Arc<dyn LanguageExtractor>) {
        let id = extractor.language_id();
        for ext in extractor.extensions() {
            self.by_ext.insert(*ext, Arc::clone(&extractor));
        }
        self.by_id.insert(id, extractor);
    }

    pub fn by_extension(&self, ext: &str) -> Option<&dyn LanguageExtractor> {
        self.by_ext.get(ext).map(|a| a.as_ref())
    }

    pub fn by_id(&self, id: LanguageId) -> Option<&dyn LanguageExtractor> {
        self.by_id.get(&id).map(|a| a.as_ref())
    }

    pub fn language_for_extension(&self, ext: &str) -> Option<LanguageId> {
        self.by_extension(ext).map(|e| e.language_id())
    }

    pub fn supported_languages(&self) -> Vec<LanguageId> {
        let mut v: Vec<LanguageId> = self.by_id.keys().copied().collect();
        v.sort_by_key(|id| id.as_str());
        v
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.by_ext.keys().copied().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only extractor — backed by a real tree-sitter language so that
    /// `tree_sitter_language()` returns a usable value, but with arbitrary
    /// extensions chosen to avoid colliding with future real registrations.
    struct MockRustyExtractor;

    impl LanguageExtractor for MockRustyExtractor {
        fn language_id(&self) -> LanguageId {
            LanguageId::Rust
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["mockrs", "rsmock"]
        }
        fn tree_sitter_language(&self) -> tree_sitter::Language {
            tree_sitter_rust::language()
        }
    }

    fn registry_with_mock() -> Registry {
        let mut r = Registry::empty();
        r.register(Arc::new(MockRustyExtractor));
        r
    }

    #[test]
    fn test_lang_registry_empty_returns_none_for_any_extension() {
        let r = Registry::empty();
        assert!(r.by_extension("rs").is_none());
        assert!(r.by_extension("py").is_none());
    }

    #[test]
    fn test_lang_registry_resolves_known_extension() {
        let r = registry_with_mock();
        let e = r.by_extension("mockrs").expect("mockrs registered");
        assert_eq!(e.language_id(), LanguageId::Rust);
    }

    #[test]
    fn test_lang_registry_returns_none_for_unknown_extension() {
        let r = registry_with_mock();
        assert!(r.by_extension("py").is_none());
        assert!(r.by_extension("").is_none());
    }

    #[test]
    fn test_lang_registry_lists_all_supported_languages() {
        let r = registry_with_mock();
        assert_eq!(r.supported_languages(), vec![LanguageId::Rust]);
    }

    #[test]
    fn test_lang_registry_lists_all_supported_extensions_sorted() {
        let r = registry_with_mock();
        assert_eq!(r.supported_extensions(), vec!["mockrs", "rsmock"]);
    }

    #[test]
    fn test_lang_registry_by_id_resolves_to_registered_extractor() {
        let r = registry_with_mock();
        let e = r.by_id(LanguageId::Rust).expect("rust registered");
        assert_eq!(e.extensions(), &["mockrs", "rsmock"]);
    }

    #[test]
    fn test_lang_registry_by_id_returns_none_for_unregistered() {
        let r = registry_with_mock();
        assert!(r.by_id(LanguageId::Python).is_none());
    }

    #[test]
    fn test_lang_registry_language_for_extension_returns_id() {
        let r = registry_with_mock();
        assert_eq!(r.language_for_extension("mockrs"), Some(LanguageId::Rust));
        assert_eq!(r.language_for_extension("nope"), None);
    }

    #[test]
    fn test_lang_registry_with_default_languages_includes_rust() {
        let r = Registry::with_default_languages();
        assert_eq!(r.language_for_extension("rs"), Some(LanguageId::Rust));
    }

    #[test]
    fn test_lang_registry_with_default_languages_includes_python() {
        let r = Registry::with_default_languages();
        assert_eq!(r.language_for_extension("py"), Some(LanguageId::Python));
    }

    #[test]
    fn test_lang_registry_with_default_languages_lists_all_languages_sorted() {
        let r = Registry::with_default_languages();
        // `supported_languages` sorts by `as_str()` — alphabetical:
        // "dart" < "go" < "javascript" < "python" < "rust" < "typescript".
        assert_eq!(
            r.supported_languages(),
            vec![
                LanguageId::Dart,
                LanguageId::Go,
                LanguageId::JavaScript,
                LanguageId::Python,
                LanguageId::Rust,
                LanguageId::TypeScript,
            ]
        );
    }

    #[test]
    fn test_lang_registry_with_default_languages_includes_typescript_for_ts_and_tsx() {
        let r = Registry::with_default_languages();
        assert_eq!(r.language_for_extension("ts"), Some(LanguageId::TypeScript));
        assert_eq!(r.language_for_extension("tsx"), Some(LanguageId::TypeScript));
    }

    #[test]
    fn test_lang_registry_with_default_languages_includes_javascript_extensions() {
        let r = Registry::with_default_languages();
        assert_eq!(r.language_for_extension("js"), Some(LanguageId::JavaScript));
        assert_eq!(r.language_for_extension("jsx"), Some(LanguageId::JavaScript));
        assert_eq!(r.language_for_extension("mjs"), Some(LanguageId::JavaScript));
        assert_eq!(r.language_for_extension("cjs"), Some(LanguageId::JavaScript));
    }

    #[test]
    fn test_lang_registry_with_default_languages_includes_go() {
        let r = Registry::with_default_languages();
        assert_eq!(r.language_for_extension("go"), Some(LanguageId::Go));
    }

    #[test]
    fn test_lang_registry_satisfies_send_sync_clone_bounds() {
        // T7's rayon pipeline requires Registry: Send + Sync; the empty Default
        // is also expected to be cheap because by_ext / by_id are HashMaps.
        fn assert_bounds<T: Send + Sync + Clone + Default>() {}
        assert_bounds::<Registry>();
    }

    #[test]
    fn test_lang_registry_register_same_extension_twice_overwrites() {
        // Document the silent-overwrite semantics. If a future task wants
        // first-write-wins or an error, this test forces an explicit decision.
        struct Other;
        impl LanguageExtractor for Other {
            fn language_id(&self) -> LanguageId {
                LanguageId::Python
            }
            fn extensions(&self) -> &'static [&'static str] {
                &["mockrs"] // collides with MockRustyExtractor
            }
            fn tree_sitter_language(&self) -> tree_sitter::Language {
                tree_sitter_python::language()
            }
        }
        let mut r = registry_with_mock();
        assert_eq!(r.language_for_extension("mockrs"), Some(LanguageId::Rust));
        r.register(Arc::new(Other));
        assert_eq!(
            r.language_for_extension("mockrs"),
            Some(LanguageId::Python),
            "second registration of an extension overwrites the first"
        );
    }
}
