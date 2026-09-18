//! Diff a set of [`DiscoveredFile`]s (what the walker found on disk now)
//! against the current state of the SQLite index. Drives the `lens update`
//! subcommand: only changed/new files need to be re-extracted; deleted paths
//! need their rows removed.
//!
//! ## Hash semantics
//!
//! The walker (`crate::walk::discover`) computes a blake3 content hash on
//! every file read. Storage's `files.content_hash` column persists the same
//! 32-byte digest. A path is considered:
//!
//! - **unchanged** if the on-disk hash equals the stored hash byte-for-byte;
//! - **changed** if the path exists in BOTH places but the hashes differ;
//! - **new** if the path is on disk but absent from the index;
//! - **deleted** if the path is in the index but absent from disk.
//!
//! Modification time (mtime) is intentionally NOT consulted — touching a
//! file without changing its contents should be a no-op for re-extraction.
//! Hash compare is the source of truth.
//!
//! ## Memory footprint
//!
//! `diff_against_index` loads the full `(path, content_hash)` pair list from
//! storage into a `HashMap<String, [u8; 32]>`. For a 100K-file repo at
//! ~50 bytes per entry, that's ~5 MB — acceptable for v1. A streaming /
//! cursor-based comparator can replace this if it ever becomes a bottleneck.

use std::collections::HashMap;

use rusqlite::params;

use crate::error::{LensError, Result};
use crate::storage::Storage;
use crate::walk::{DiscoveredFile, FileStamp};

/// Outcome of comparing on-disk discovery against the persisted index. Each
/// vector is sorted by `relative_path` for deterministic test output.
///
/// `changed` and `new` carry the FULL `DiscoveredFile` because the orchestrator
/// will hand them to the extraction pipeline. `deleted` and `unchanged` carry
/// only paths since no further action needs the disk state.
#[derive(Debug, Default, Clone)]
pub struct FileDiff {
    pub unchanged: Vec<String>,
    pub changed: Vec<DiscoveredFile>,
    pub new: Vec<DiscoveredFile>,
    pub deleted: Vec<String>,
    /// Unchanged files whose `(size_bytes, modified_at)` stamp drifted from
    /// the stored row (e.g. a `touch`, a checkout that rewrote identical
    /// bytes). Their hash still matches so no re-extraction is needed, but
    /// the stamp should be refreshed via [`apply_restamps`] so the next
    /// [`crate::walk::discover_with_stamps`] pass can skip reading them.
    pub restamp: Vec<(String, u64, i64)>,
}

impl FileDiff {
    /// True when there is nothing to do (no changes, no additions, no
    /// deletions). The caller can short-circuit re-extraction in this case.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.new.is_empty() && self.deleted.is_empty()
    }

    /// Total number of paths that need re-extraction (changed + new).
    pub fn extract_count(&self) -> usize {
        self.changed.len() + self.new.len()
    }
}

/// Load `(path → stamp)` for every indexed code file. Feed the result to
/// [`crate::walk::discover_with_stamps`] so unchanged files are not re-read.
///
/// Rows with a malformed hash (not 32 bytes) are omitted so the on-disk
/// file appears as "new" and gets re-extracted, healing the row.
pub fn load_file_stamps(storage: &Storage) -> Result<HashMap<String, FileStamp>> {
    let conn = storage.connection();
    let mut stmt = conn
        .prepare("SELECT path, content_hash, size_bytes, modified_at FROM files")
        .map_err(|e| LensError::other(format!("prepare stamp scan: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| LensError::other(format!("query stamp scan: {e}")))?;
    let mut map: HashMap<String, FileStamp> = HashMap::new();
    for r in rows {
        let (path, blob, size, mtime) = r.map_err(|e| LensError::other(format!("row stamp scan: {e}")))?;
        if let Ok(content_hash) = <[u8; 32]>::try_from(blob.as_slice()) {
            map.insert(path, FileStamp { size_bytes: size.max(0) as u64, modified_at: mtime, content_hash });
        }
    }
    Ok(map)
}

/// Refresh `(size_bytes, modified_at)` for the paths in `restamp`. One
/// transaction; a no-op for an empty list.
pub fn apply_restamps(storage: &mut Storage, restamp: &[(String, u64, i64)]) -> Result<u64> {
    if restamp.is_empty() {
        return Ok(0);
    }
    let tx = storage.transaction()?;
    let mut n: u64 = 0;
    {
        let mut stmt = tx
            .prepare("UPDATE files SET size_bytes = ?1, modified_at = ?2 WHERE path = ?3")
            .map_err(|e| LensError::other(format!("prepare restamp: {e}")))?;
        for (path, size, mtime) in restamp {
            n += stmt
                .execute(params![*size as i64, *mtime, path])
                .map_err(|e| LensError::other(format!("restamp {path}: {e}")))? as u64;
        }
    }
    tx.commit().map_err(|e| LensError::other(format!("commit restamp: {e}")))?;
    Ok(n)
}

/// Compute the diff between `discovered` (current disk state) and the persisted
/// index in `storage`. Pure read operation — does not mutate the index.
pub fn diff_against_index(storage: &Storage, discovered: &[DiscoveredFile]) -> Result<FileDiff> {
    let mut indexed = load_file_stamps(storage)?;

    let mut diff = FileDiff::default();
    for d in discovered {
        match indexed.remove(&d.relative_path) {
            Some(stamp) => {
                if stamp.content_hash == d.content_hash {
                    diff.unchanged.push(d.relative_path.clone());
                    if stamp.size_bytes != d.size_bytes || stamp.modified_at != d.modified_at {
                        diff.restamp.push((d.relative_path.clone(), d.size_bytes, d.modified_at));
                    }
                } else {
                    diff.changed.push(d.clone());
                }
            }
            None => diff.new.push(d.clone()),
        }
    }

    // Anything left in `indexed` is on the index but not on disk — deleted.
    diff.deleted.extend(indexed.into_keys());

    diff.unchanged.sort();
    diff.restamp.sort();
    diff.changed.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    diff.new.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    diff.deleted.sort();

    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ExtractedFile;
    use crate::lang::LanguageId;
    use crate::storage::insert::insert_extracted_files;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn tmp_storage() -> (TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("index.db");
        let storage = Storage::open(&path).expect("open");
        (dir, storage)
    }

    fn rust_file(path: &str, hash: [u8; 32]) -> ExtractedFile {
        let mut ef = ExtractedFile::empty(path, LanguageId::Rust);
        ef.content_hash = hash;
        ef.size_bytes = 100;
        ef.modified_at = 1700000000;
        ef
    }

    fn discovered(path: &str, hash: [u8; 32]) -> DiscoveredFile {
        DiscoveredFile {
            relative_path: path.into(),
            absolute_path: PathBuf::from("/tmp").join(path),
            language: LanguageId::Rust,
            content_hash: hash,
            size_bytes: 100,
            modified_at: 1700000000,
        }
    }

    #[test]
    fn test_diff_returns_unchanged_when_hashes_match() {
        let (_g, mut s) = tmp_storage();
        let h = [7u8; 32];
        insert_extracted_files(&mut s, &[rust_file("src/a.rs", h)]).unwrap();
        let diff = diff_against_index(&s, &[discovered("src/a.rs", h)]).unwrap();
        assert_eq!(diff.unchanged, vec!["src/a.rs"]);
        assert!(diff.changed.is_empty());
        assert!(diff.new.is_empty());
        assert!(diff.deleted.is_empty());
        assert!(diff.is_empty());
    }

    #[test]
    fn test_diff_returns_changed_when_hash_mismatches() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(&mut s, &[rust_file("src/a.rs", [1u8; 32])]).unwrap();
        let diff = diff_against_index(&s, &[discovered("src/a.rs", [2u8; 32])]).unwrap();
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].relative_path, "src/a.rs");
        assert!(diff.unchanged.is_empty());
        assert!(diff.new.is_empty());
        assert!(diff.deleted.is_empty());
        assert!(!diff.is_empty());
    }

    #[test]
    fn test_diff_returns_new_when_path_absent_from_index() {
        let (_g, s) = tmp_storage();
        let diff = diff_against_index(&s, &[discovered("src/new.rs", [3u8; 32])]).unwrap();
        assert_eq!(diff.new.len(), 1);
        assert_eq!(diff.new[0].relative_path, "src/new.rs");
        assert!(diff.unchanged.is_empty());
        assert!(diff.changed.is_empty());
        assert!(diff.deleted.is_empty());
    }

    #[test]
    fn test_diff_returns_deleted_when_indexed_path_absent_from_disk() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(&mut s, &[rust_file("src/gone.rs", [4u8; 32])]).unwrap();
        let diff = diff_against_index(&s, &[]).unwrap();
        assert_eq!(diff.deleted, vec!["src/gone.rs"]);
        assert!(diff.unchanged.is_empty());
        assert!(diff.changed.is_empty());
        assert!(diff.new.is_empty());
    }

    #[test]
    fn test_diff_handles_mixed_buckets_in_one_call() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(
            &mut s,
            &[
                rust_file("src/keep.rs", [1u8; 32]),
                rust_file("src/touch.rs", [2u8; 32]),
                rust_file("src/gone.rs", [3u8; 32]),
            ],
        )
        .unwrap();
        let diff = diff_against_index(
            &s,
            &[
                discovered("src/keep.rs", [1u8; 32]), // unchanged
                discovered("src/touch.rs", [99u8; 32]), // changed
                discovered("src/fresh.rs", [4u8; 32]), // new
            ],
        )
        .unwrap();
        assert_eq!(diff.unchanged, vec!["src/keep.rs"]);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].relative_path, "src/touch.rs");
        assert_eq!(diff.new.len(), 1);
        assert_eq!(diff.new[0].relative_path, "src/fresh.rs");
        assert_eq!(diff.deleted, vec!["src/gone.rs"]);
        assert_eq!(diff.extract_count(), 2);
    }

    #[test]
    fn test_diff_empty_inputs_returns_all_empty_buckets() {
        let (_g, s) = tmp_storage();
        let diff = diff_against_index(&s, &[]).unwrap();
        assert!(diff.is_empty());
        assert_eq!(diff.unchanged.len(), 0);
    }

    #[test]
    fn test_diff_deterministic_ordering() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(
            &mut s,
            &[
                rust_file("z.rs", [1u8; 32]),
                rust_file("a.rs", [1u8; 32]),
                rust_file("m.rs", [1u8; 32]),
            ],
        )
        .unwrap();
        // Discovery is in random order — we expect the diff buckets sorted.
        let diff = diff_against_index(
            &s,
            &[
                discovered("m.rs", [1u8; 32]),
                discovered("z.rs", [99u8; 32]),
                discovered("a.rs", [1u8; 32]),
            ],
        )
        .unwrap();
        assert_eq!(diff.unchanged, vec!["a.rs", "m.rs"]);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].relative_path, "z.rs");
    }

    #[test]
    fn test_diff_treats_malformed_hash_as_forced_reextract() {
        // Insert a row through raw SQL with a wrong-length content_hash to
        // simulate a corrupted DB. The diff must surface that path as "new"
        // (so update_files will overwrite it with a correct row), not panic.
        let (_g, s) = tmp_storage();
        s.connection()
            .execute(
                "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
                 VALUES (?1, 'rust', ?2, 1, 1, 1)",
                rusqlite::params!["src/corrupt.rs", &[0u8; 8] as &[u8]],
            )
            .unwrap();
        let diff = diff_against_index(&s, &[discovered("src/corrupt.rs", [9u8; 32])]).unwrap();
        // Malformed row was skipped by the loader → on-disk path appears as new.
        assert_eq!(diff.new.len(), 1);
        assert_eq!(diff.new[0].relative_path, "src/corrupt.rs");
    }

    #[test]
    fn test_file_diff_is_empty_only_when_no_work_pending() {
        let mut d = FileDiff::default();
        assert!(d.is_empty());
        d.unchanged.push("x".into());
        assert!(d.is_empty(), "unchanged alone is empty (no work)");
        d.changed.push(discovered("y.rs", [0u8; 32]));
        assert!(!d.is_empty());
    }

    #[test]
    fn test_diff_does_not_mutate_storage() {
        let (_g, mut s) = tmp_storage();
        insert_extracted_files(&mut s, &[rust_file("x.rs", [5u8; 32])]).unwrap();
        let pre: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        let _ = diff_against_index(&s, &[discovered("y.rs", [6u8; 32])]).unwrap();
        let post: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pre, post, "diff must not mutate storage");
    }
    #[test]
    fn test_diff_reports_restamp_for_unchanged_hash_with_drifted_stamp() {
        let (_g, mut s) = tmp_storage();
        let h = [7u8; 32];
        insert_extracted_files(&mut s, &[rust_file("src/a.rs", h)]).unwrap();
        let mut d = discovered("src/a.rs", h);
        d.modified_at += 100; // touched, same content
        let diff = diff_against_index(&s, &[d]).unwrap();
        assert_eq!(diff.unchanged, vec!["src/a.rs"]);
        assert_eq!(diff.restamp.len(), 1);
        assert!(diff.is_empty(), "restamp alone is not re-extraction work");
        let n = apply_restamps(&mut s, &diff.restamp).unwrap();
        assert_eq!(n, 1);
        let stamps = load_file_stamps(&s).unwrap();
        assert_eq!(stamps["src/a.rs"].modified_at, 1700000100);
    }

    #[test]
    fn test_load_file_stamps_omits_malformed_hash_rows() {
        let (_g, s) = tmp_storage();
        s.connection()
            .execute(
                "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
                 VALUES ('bad.rs', 'rust', ?1, 1, 1, 1)",
                rusqlite::params![&[0u8; 8] as &[u8]],
            )
            .unwrap();
        assert!(load_file_stamps(&s).unwrap().is_empty());
    }
}
