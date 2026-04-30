//! Indexing pipeline. Composes [`crate::walk::discover`], [`crate::parse::parse`],
//! and per-language [`crate::lang::LanguageExtractor::extract`] into a single
//! parallel run.
//!
//! Concurrency strategy: rayon `par_iter` over discovered files. Each worker
//! reuses the per-thread `tree_sitter::Parser` pool from
//! [`crate::parse`]. The output is sorted by `relative_path` so test assertions
//! and downstream merges are deterministic regardless of scheduler order.
//!
//! Failure mode: fail-fast. The first worker to return an error short-circuits
//! the collect; remaining workers may still complete but their results are
//! discarded. Errors are typed [`crate::error::LensError`].

use std::path::Path;

use rayon::prelude::*;

use crate::error::{LensError, Result};
use crate::extract::{ExtractContext, ExtractedFile};
use crate::lang::Registry;
use crate::parse::parse;
use crate::walk::discover;

/// Walk `root` for files registered in `registry`, parse each, and run the
/// per-language extractor. Returns one [`ExtractedFile`] per discovered file,
/// sorted by `relative_path`.
pub fn run(root: &Path, registry: &Registry) -> Result<Vec<ExtractedFile>> {
    let discovered = discover(root, registry)?;
    run_on_discovered(&discovered, registry)
}

/// Pipeline variant that operates on a pre-built `DiscoveredFile` list rather
/// than walking from a root. Used by the incremental `lens update` flow, which
/// walks once, diffs against the index, then re-extracts only the changed/new
/// subset.
///
/// Output is sorted by `relative_path` for deterministic downstream merges,
/// matching [`run`].
pub fn run_on_discovered(
    discovered: &[crate::walk::DiscoveredFile],
    registry: &Registry,
) -> Result<Vec<ExtractedFile>> {
    let mut extracted: Vec<ExtractedFile> = discovered
        .par_iter()
        .map(|df| -> Result<ExtractedFile> {
            let extractor = registry.by_id(df.language).ok_or_else(|| {
                LensError::other(format!(
                    "no extractor registered for language {} (file: {})",
                    df.language, df.relative_path
                ))
            })?;
            let bytes = std::fs::read(&df.absolute_path)
                .map_err(|e| LensError::io_at(&df.absolute_path, e))?;
            let parsed = parse(bytes, extractor)?;
            let module_path = extractor.module_path_from_relative_path(&df.relative_path);
            let ctx = ExtractContext {
                relative_path: &df.relative_path,
                module_path: &module_path,
            };
            let mut ef = extractor.extract(&parsed, &ctx);
            // Carry per-file metadata from discovery into the extracted record
            // so the storage layer can populate the `files` table without
            // re-walking the project. The extractor itself never sees this
            // data — it's spliced in here at the pipeline boundary.
            ef.content_hash = df.content_hash;
            ef.size_bytes = df.size_bytes;
            ef.modified_at = df.modified_at;
            Ok(ef)
        })
        .collect::<Result<Vec<_>>>()?;

    // rayon yields workers in non-deterministic order. Sort the output so
    // tests and downstream merges see a stable shape.
    extracted.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(extracted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::LanguageId;

    fn fixtures_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn test_pipeline_handles_empty_directory() {
        let dir = fixtures_root();
        let r = Registry::with_default_languages();
        let out = run(dir.path(), &r).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn test_pipeline_runs_on_rust_only_tempdir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"pub fn hello() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), b"pub struct Point;\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].relative_path, "a.rs");
        assert_eq!(out[0].language, LanguageId::Rust);
        assert!(out[0].symbols.iter().any(|s| s.name == "hello"));
        assert_eq!(out[1].relative_path, "b.rs");
        assert!(out[1].symbols.iter().any(|s| s.name == "Point"));
    }

    #[test]
    fn test_pipeline_runs_on_python_only_tempdir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("a.py"), b"def hello():\n    pass\n").unwrap();
        std::fs::write(root.join("b.py"), b"class Point:\n    pass\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].relative_path, "a.py");
        assert_eq!(out[0].language, LanguageId::Python);
        assert!(out[0].symbols.iter().any(|s| s.name == "hello"));
        assert_eq!(out[1].relative_path, "b.py");
        assert!(out[1].symbols.iter().any(|s| s.name == "Point"));
    }

    #[test]
    fn test_pipeline_runs_on_mixed_rust_and_python_tempdir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"pub fn r_fn() {}\n").unwrap();
        std::fs::write(root.join("b.py"), b"def p_fn():\n    pass\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();

        assert_eq!(out.len(), 2);
        let by_path: std::collections::HashMap<&str, &ExtractedFile> =
            out.iter().map(|f| (f.relative_path.as_str(), f)).collect();
        assert_eq!(by_path["a.rs"].language, LanguageId::Rust);
        assert_eq!(by_path["b.py"].language, LanguageId::Python);
        assert!(by_path["a.rs"].symbols.iter().any(|s| s.name == "r_fn"));
        assert!(by_path["b.py"].symbols.iter().any(|s| s.name == "p_fn"));
    }

    #[test]
    fn test_pipeline_output_sorted_by_relative_path() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("z.rs"), b"pub fn z() {}\n").unwrap();
        std::fs::write(root.join("a.rs"), b"pub fn a() {}\n").unwrap();
        std::fs::write(root.join("m.rs"), b"pub fn m() {}\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        let paths: Vec<&str> = out.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "m.rs", "z.rs"]);
    }

    #[test]
    fn test_pipeline_returns_err_on_nonexistent_root() {
        let r = Registry::with_default_languages();
        let res = run(Path::new("/tmp/lens-pipeline-nope-xyz999"), &r);
        assert!(res.is_err());
    }

    #[test]
    fn test_pipeline_skips_files_with_unregistered_extension() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"pub fn rs() {}\n").unwrap();
        std::fs::write(root.join("b.cobol"), b"PROGRAM-ID. unused.\n").unwrap();
        std::fs::write(root.join("c.txt"), b"plain text\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        let paths: Vec<&str> = out.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs"]);
    }

    #[test]
    fn test_pipeline_module_path_for_rust_lib_rs_collapses_to_parent_dir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("lib.rs"), b"pub fn lib_root() {}\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 1);
        let sym = out[0]
            .symbols
            .iter()
            .find(|s| s.name == "lib_root")
            .expect("lib_root symbol");
        // module_path = "src" (collapsed), so qname = "src::lib_root".
        assert_eq!(sym.qualified_name, "src::lib_root");
    }

    #[test]
    fn test_pipeline_module_path_for_rust_mod_rs_collapses_to_parent_dir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src").join("foo")).unwrap();
        std::fs::write(
            root.join("src").join("foo").join("mod.rs"),
            b"pub fn helper() {}\n",
        )
        .unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 1);
        let sym = out[0]
            .symbols
            .iter()
            .find(|s| s.name == "helper")
            .expect("helper symbol");
        // module_path = "src::foo" (collapsed), so qname = "src::foo::helper".
        assert_eq!(sym.qualified_name, "src::foo::helper");
    }

    #[test]
    fn test_pipeline_module_path_for_python_init_py_collapses_to_parent_dir() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::write(
            root.join("pkg").join("__init__.py"),
            b"def init_helper():\n    pass\n",
        )
        .unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 1);
        let sym = out[0]
            .symbols
            .iter()
            .find(|s| s.name == "init_helper")
            .expect("init_helper symbol");
        // module_path = "pkg" (collapsed), so qname = "pkg.init_helper".
        assert_eq!(sym.qualified_name, "pkg.init_helper");
    }

    #[test]
    fn test_pipeline_module_path_for_regular_python_file_uses_dotted_path() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg").join("sub")).unwrap();
        std::fs::write(
            root.join("pkg").join("sub").join("util.py"),
            b"def f():\n    pass\n",
        )
        .unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 1);
        let sym = out[0].symbols.iter().find(|s| s.name == "f").unwrap();
        assert_eq!(sym.qualified_name, "pkg.sub.util.f");
    }

    #[test]
    fn test_pipeline_recurses_into_subdirectories() {
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        std::fs::write(root.join("a").join("b").join("c.rs"), b"pub fn deep() {}\n").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].relative_path, "a/b/c.rs");
        let sym = out[0].symbols.iter().find(|s| s.name == "deep").unwrap();
        assert_eq!(sym.qualified_name, "a::b::c::deep");
    }

    #[test]
    fn test_pipeline_handles_zero_byte_source_file() {
        // Tree-sitter must parse an empty source successfully (returns an
        // empty `module`/`source_file` root); the extractor must yield an
        // ExtractedFile with no symbols, refs, calls, imports, or
        // type_relations.
        let dir = fixtures_root();
        let root = dir.path();
        std::fs::write(root.join("empty.rs"), b"").unwrap();
        std::fs::write(root.join("empty.py"), b"").unwrap();

        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 2);
        for f in &out {
            assert!(f.symbols.is_empty(), "{}: expected no symbols", f.relative_path);
            assert!(f.refs.is_empty());
            assert!(f.calls.is_empty());
            assert!(f.imports.is_empty());
            assert!(f.type_relations.is_empty());
        }
    }

    #[test]
    fn test_pipeline_handles_many_files_in_parallel() {
        // Stress: 50 files. Verifies par_iter doesn't deadlock or skip files.
        let dir = fixtures_root();
        let root = dir.path();
        for i in 0..50u32 {
            std::fs::write(
                root.join(format!("f{i:02}.rs")),
                format!("pub fn f{i}() {{}}\n").as_bytes(),
            )
            .unwrap();
        }
        let r = Registry::with_default_languages();
        let out = run(root, &r).unwrap();
        assert_eq!(out.len(), 50);
        // Every file must contribute exactly its one fn symbol.
        for (i, f) in out.iter().enumerate() {
            assert_eq!(f.relative_path, format!("f{i:02}.rs"));
            let expected_name = format!("f{i}");
            assert!(
                f.symbols.iter().any(|s| s.name == expected_name),
                "expected fn {expected_name} in {}",
                f.relative_path
            );
        }
    }
}
