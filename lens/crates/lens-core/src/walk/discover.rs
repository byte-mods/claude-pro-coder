use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::error::{LensError, Result};
use crate::lang::{LanguageId, Registry};

/// One file discovered by the walker. Field order mirrors the `files` table
/// in the SQLite schema (minus `indexed_at`, which is assigned at insert time
/// by the storage layer in Section 4).
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Project-relative path with forward-slash separators (canonical text
    /// form for storage).
    pub relative_path: String,
    /// Absolute path on disk (used by the parser for I/O during extraction).
    pub absolute_path: PathBuf,
    /// Registry-resolved language identifier.
    pub language: LanguageId,
    /// blake3 hash of file contents — exactly 32 bytes, matching the
    /// `files.content_hash BLOB` column shape.
    pub content_hash: [u8; 32],
    pub size_bytes: u64,
    /// Last-modified time as Unix epoch seconds.
    pub modified_at: i64,
}

/// Walk `root` recursively, respecting `.gitignore` and skipping `.lens/`.
/// Files whose extension is not registered are silently dropped (the walker
/// never decides to add a language; that's the registry's job). Returned
/// vector is sorted by `relative_path` for deterministic test output.
///
/// **TOCTOU note (v1).** This function calls `std::fs::metadata` and then
/// `std::fs::read` on the same path; the file may be modified or removed
/// between the two calls. The resulting `size_bytes` and `content_hash` will
/// disagree if the race occurs. v1 accepts this (offline indexing); a future
/// version may either lock the file or read first and derive size from the
/// buffer.
///
/// **Large-file note (v1).** Each file is read into memory in full via
/// `std::fs::read`. There is no streaming or memory cap. Files larger than
/// the host's available RAM will cause an allocation failure. v1 targets
/// source-tree files (typically under a few MB); a streaming or
/// chunked-hashing path is left to a future version.
///
/// **Hidden-file note.** `ignore::WalkBuilder` skips dotfiles by default
/// (e.g. `.foo.rs`). v1 keeps this default — there is no flag to opt in.
pub fn discover(root: &Path, registry: &Registry) -> Result<Vec<DiscoveredFile>> {
    if !root.exists() {
        return Err(LensError::invalid_path(root, "does not exist"));
    }
    if !root.is_dir() {
        return Err(LensError::invalid_path(root, "not a directory"));
    }

    // Defence in depth: even if the user's .gitignore omits `.lens/`, we
    // refuse to index our own state directory.
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    overrides
        .add("!.lens/")
        .map_err(|e| LensError::other(format!("override builder add: {e}")))?;
    let overrides = overrides
        .build()
        .map_err(|e| LensError::other(format!("override builder build: {e}")))?;

    // `require_git(false)` makes WalkBuilder honour `.gitignore` even outside
    // an initialised git repo — the file's intent is independent of whether
    // `git init` has been run.
    let walker = WalkBuilder::new(root)
        .follow_links(false)
        .require_git(false)
        .overrides(overrides)
        .build();

    let mut out = Vec::new();
    for entry in walker {
        let dent = match entry {
            Ok(d) => d,
            Err(e) => return Err(LensError::other(format!("walk error: {e}"))),
        };

        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }

        let abs = dent.path().to_path_buf();
        let ext = match abs.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let lang = match registry.language_for_extension(ext) {
            Some(l) => l,
            None => continue,
        };

        let rel = relative_path_str(root, &abs)?;
        let metadata = std::fs::metadata(&abs).map_err(|e| LensError::io_at(&abs, e))?;
        let size_bytes = metadata.len();
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let bytes = std::fs::read(&abs).map_err(|e| LensError::io_at(&abs, e))?;
        let content_hash = *blake3::hash(&bytes).as_bytes();

        out.push(DiscoveredFile {
            relative_path: rel,
            absolute_path: abs,
            language: lang,
            content_hash,
            size_bytes,
            modified_at,
        });
    }

    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

fn relative_path_str(root: &Path, abs: &Path) -> Result<String> {
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| LensError::invalid_path(abs, "outside project root"))?;
    let s = rel
        .to_str()
        .ok_or_else(|| LensError::invalid_path(abs, "path is not valid utf-8"))?;
    Ok(s.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::lang::LanguageExtractor;

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

    fn rust_only_registry() -> Registry {
        let mut r = Registry::empty();
        r.register(Arc::new(StubRust));
        r
    }

    #[test]
    fn test_walk_discovers_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(root.join("b.rs"), b"fn b() {}").unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "b.rs"]);
        for f in &files {
            assert_eq!(f.language, LanguageId::Rust);
        }
    }

    #[test]
    fn test_walk_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(root.join("ignored.rs"), b"fn ignored() {}").unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn test_walk_skips_unknown_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(root.join("b.cobol"), b"PROGRAM-ID.").unwrap();
        std::fs::write(root.join("c.txt"), b"text").unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn test_walk_returns_blake3_32_byte_hash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = b"fn x() {}\n";
        std::fs::write(root.join("a.rs"), content).unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        assert_eq!(files.len(), 1);
        let expected = *blake3::hash(content).as_bytes();
        assert_eq!(files[0].content_hash, expected);
        assert_eq!(files[0].content_hash.len(), 32);
        assert_eq!(files[0].size_bytes, content.len() as u64);
    }

    #[test]
    fn test_walk_handles_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let r = rust_only_registry();
        let files = discover(dir.path(), &r).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_walk_skips_lens_dir_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::create_dir_all(root.join(".lens")).unwrap();
        std::fs::write(root.join(".lens").join("ghost.rs"), b"fn ghost() {}").unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs"], "files under .lens/ must be omitted");
    }

    #[test]
    fn test_walk_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(root.join("sub").join("b.rs"), b"fn b() {}").unwrap();
        std::fs::write(root.join("sub").join("deep").join("c.rs"), b"fn c() {}").unwrap();

        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "sub/b.rs", "sub/deep/c.rs"]);
    }

    #[test]
    fn test_walk_returns_err_on_nonexistent_root() {
        let r = rust_only_registry();
        let result = discover(Path::new("/tmp/lens-test-this-does-not-exist-xyz123"), &r);
        assert!(result.is_err());
    }

    #[test]
    fn test_walk_returns_err_on_file_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, b"fn a() {}").unwrap();
        let r = rust_only_registry();
        let result = discover(&file, &r);
        assert!(result.is_err());
    }

    #[test]
    fn test_walk_modified_at_is_unix_seconds_in_plausible_range() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let mt = files[0].modified_at;
        assert!(
            (1_577_836_800..4_102_444_800).contains(&mt),
            "implausible mtime: {mt}"
        );
    }

    #[test]
    fn test_walk_output_sorted_by_relative_path_for_determinism() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("z.rs"), b"").unwrap();
        std::fs::write(root.join("a.rs"), b"").unwrap();
        std::fs::write(root.join("m.rs"), b"").unwrap();
        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "m.rs", "z.rs"]);
    }

    #[test]
    fn test_walk_skips_files_with_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Makefile"), b"all:").unwrap();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        let r = rust_only_registry();
        let files = discover(root, &r).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert_eq!(names, vec!["a.rs"]);
    }

    #[test]
    fn test_walk_empty_registry_drops_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), b"fn a() {}").unwrap();
        std::fs::write(root.join("b.py"), b"def b(): pass").unwrap();
        let r = Registry::empty();
        let files = discover(root, &r).unwrap();
        assert!(files.is_empty(), "empty registry must yield empty discovery");
    }
}
