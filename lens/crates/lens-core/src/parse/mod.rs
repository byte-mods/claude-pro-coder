//! Tree-sitter wrapper. Owns a per-thread, per-language `tree_sitter::Parser`
//! pool so that the parallel pipeline in T7 can reuse parsers across files
//! within a worker thread without paying the `set_language` cost each call.
//!
//! `tree_sitter::Parser` is `Send` but not `Sync`; the per-thread pool keeps
//! us correct without locking.
//!
//! **Pool growth bound.** The pool grows at most to one parser per
//! [`crate::lang::LanguageId`] variant per thread. With N rayon workers and L
//! languages, total parser memory is bounded by `N * L`. There is no
//! eviction — parsers live for the thread's lifetime. This is fine for v1's
//! small `LanguageId` set; if `LanguageId` ever grows large, an LRU eviction
//! step would belong here.
//!
//! **Multi-thread test rigor.** [`tests::test_parse_works_in_multiple_threads_independently`]
//! spawns 4 threads. It verifies absence of obvious data races (each parser
//! is thread-local), but does **not** prove freedom from subtle ordering
//! bugs under high contention. Property-based or loom-style stress is left
//! to a future task.

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;

use tree_sitter::{Parser as TsParser, Tree};

use crate::error::{LensError, Result};
use crate::lang::{LanguageExtractor, LanguageId};

/// A parsed source file. Owns both the tree and the source bytes so that
/// `Node::byte_range` slices remain valid for the lifetime of the value.
pub struct ParsedFile {
    tree: Tree,
    source: Vec<u8>,
}

impl ParsedFile {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn source(&self) -> &[u8] {
        &self.source
    }

    pub fn root_node(&self) -> tree_sitter::Node<'_> {
        self.tree.root_node()
    }

    pub fn has_error(&self) -> bool {
        self.root_node().has_error()
    }
}

thread_local! {
    static PARSER_POOL: RefCell<HashMap<LanguageId, TsParser>> = RefCell::new(HashMap::new());
}

/// Parse `source` with the given language. Tree-sitter recovers from syntax
/// errors and returns a partial tree; callers should check
/// [`ParsedFile::has_error`] if they need to reject malformed input.
pub fn parse(source: Vec<u8>, lang: &dyn LanguageExtractor) -> Result<ParsedFile> {
    let id = lang.language_id();
    PARSER_POOL.with(|cell| {
        let mut pool = cell.borrow_mut();
        let parser = match pool.entry(id) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => {
                let mut p = TsParser::new();
                p.set_language(&lang.tree_sitter_language())
                    .map_err(|e| LensError::other(format!("set_language for {id}: {e}")))?;
                v.insert(p)
            }
        };
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| LensError::other(format!("parse returned None for {id}")))?;
        Ok(ParsedFile { tree, source })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRust;
    impl LanguageExtractor for StubRust {
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

    struct StubPython;
    impl LanguageExtractor for StubPython {
        fn language_id(&self) -> LanguageId {
            LanguageId::Python
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["py"]
        }
        fn tree_sitter_language(&self) -> tree_sitter::Language {
            tree_sitter_python::language()
        }
    }

    #[test]
    fn test_parse_returns_tree_for_valid_rust() {
        let pf = parse(b"fn hello() {}\n".to_vec(), &StubRust).unwrap();
        let root = pf.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!pf.has_error());
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_handles_empty_source() {
        let pf = parse(Vec::new(), &StubRust).unwrap();
        assert_eq!(pf.root_node().kind(), "source_file");
        assert!(pf.source().is_empty());
    }

    #[test]
    fn test_parse_returns_partial_tree_on_syntax_error() {
        let pf = parse(b"fn broken( {\n}".to_vec(), &StubRust).unwrap();
        assert_eq!(pf.root_node().kind(), "source_file");
        assert!(pf.has_error(), "tree-sitter must report a syntax error");
    }

    #[test]
    fn test_parse_python_via_same_api_returns_module_root() {
        let pf = parse(b"def hello(): pass\n".to_vec(), &StubPython).unwrap();
        assert_eq!(pf.root_node().kind(), "module");
        assert!(!pf.has_error());
    }

    #[test]
    fn test_parse_two_languages_in_same_thread_share_pool_correctly() {
        let r = parse(b"fn a() {}".to_vec(), &StubRust).unwrap();
        let p = parse(b"def a(): pass".to_vec(), &StubPython).unwrap();
        assert_eq!(r.root_node().kind(), "source_file");
        assert_eq!(p.root_node().kind(), "module");
    }

    #[test]
    fn test_parse_repeated_calls_reuse_pooled_parser() {
        for _ in 0..3 {
            let pf = parse(b"fn x() {}".to_vec(), &StubRust).unwrap();
            assert_eq!(pf.root_node().kind(), "source_file");
        }
    }

    #[test]
    fn test_parse_works_in_multiple_threads_independently() {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                std::thread::spawn(move || {
                    let src = format!("fn t{i}() {{}}");
                    let pf = parse(src.into_bytes(), &StubRust).unwrap();
                    assert_eq!(pf.root_node().kind(), "source_file");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_parse_source_accessor_returns_input_bytes_unchanged() {
        let src = b"fn keep() {}\n".to_vec();
        let pf = parse(src.clone(), &StubRust).unwrap();
        assert_eq!(pf.source(), src.as_slice());
    }

    #[test]
    fn test_parsed_file_is_send() {
        // T7 will move ParsedFile across rayon worker boundaries.
        fn assert_send<T: Send>() {}
        assert_send::<ParsedFile>();
    }
}
