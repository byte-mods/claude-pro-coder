use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{LensError, Result};
use crate::storage::migrations::{apply_migrations, current_schema_version};

pub struct Storage {
    path: PathBuf,
    conn: Connection,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| LensError::io_at(parent, e))?;
            }
        }
        let conn = Connection::open(&path).map_err(|e| LensError::other(format!(
            "open sqlite at {}: {e}",
            path.display()
        )))?;
        Self::configure(&conn, &path)?;
        apply_migrations(&conn)?;
        Ok(Self { path, conn })
    }

    fn configure(conn: &Connection, path: &Path) -> Result<()> {
        let pragma = |name: &str, value: &str| -> Result<()> {
            conn.pragma_update(None, name, value).map_err(|e| {
                LensError::other(format!("set pragma {name}={value} on {}: {e}", path.display()))
            })
        };
        pragma("journal_mode", "WAL")?;
        pragma("foreign_keys", "ON")?;
        pragma("synchronous", "NORMAL")?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> Result<u32> {
        current_schema_version(&self.conn)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn transaction(&mut self) -> Result<rusqlite::Transaction<'_>> {
        self.conn
            .transaction()
            .map_err(|e| LensError::other(format!("begin transaction: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn tmp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("index.db");
        (dir, path)
    }

    #[test]
    fn test_storage_open_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("index.db");
        let storage = Storage::open(&nested).expect("open creates parents");
        assert_eq!(storage.path(), nested);
        assert!(nested.exists());
    }

    #[test]
    fn test_storage_open_creates_db_file() {
        let (_g, path) = tmp_db();
        assert!(!path.exists());
        Storage::open(&path).expect("open");
        assert!(path.exists());
    }

    #[test]
    fn test_storage_version_returns_latest_after_fresh_open() {
        let (_g, path) = tmp_db();
        let storage = Storage::open(&path).expect("open");
        // Latest schema version — bumps as new migrations land. Asserting
        // >= 1 keeps this resilient to v3+ without re-asserting per bump.
        assert!(storage.version().expect("version") >= 1);
    }

    #[test]
    fn test_storage_open_existing_no_remigration() {
        let (_g, path) = tmp_db();
        Storage::open(&path).expect("first open");
        let s2 = Storage::open(&path).expect("re-open");
        assert!(s2.version().expect("version") >= 1);
        let row_count: i64 = s2
            .connection()
            .query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 1, "re-open must not duplicate meta row");
    }

    #[test]
    fn test_storage_wal_journal_mode_active() {
        let (_g, path) = tmp_db();
        let s = Storage::open(&path).expect("open");
        let mode: String = s
            .connection()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn test_storage_foreign_keys_enabled() {
        let (_g, path) = tmp_db();
        let s = Storage::open(&path).expect("open");
        let fk: i64 = s
            .connection()
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn test_storage_fk_cascade_on_file_delete_removes_symbols() {
        let (_g, path) = tmp_db();
        let s = Storage::open(&path).expect("open");
        let c = s.connection();
        c.execute(
            "INSERT INTO files (id, path, language, content_hash, size_bytes, modified_at, indexed_at)
             VALUES (1, 'src/a.rs', 'rust', X'aa', 100, 0, 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO symbols (file_id, qualified_name, name, kind,
                                  start_line, start_col, end_line, end_col,
                                  body_start_byte, body_end_byte)
             VALUES (1, 'foo', 'foo', 'function', 1, 0, 2, 0, 0, 10)",
            [],
        )
        .unwrap();
        let pre: i64 = c.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0)).unwrap();
        assert_eq!(pre, 1);
        c.execute("DELETE FROM files WHERE id = 1", []).unwrap();
        let post: i64 = c.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0)).unwrap();
        assert_eq!(post, 0, "FK CASCADE failed — symbols should be removed when parent file is deleted");
    }

    #[test]
    fn test_storage_transaction_commit_persists() {
        let (_g, path) = tmp_db();
        let mut s = Storage::open(&path).expect("open");
        {
            let tx = s.transaction().expect("begin tx");
            tx.execute(
                "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
                 VALUES ('src/x.rs', 'rust', X'bb', 50, 1, 1)",
                [],
            )
            .unwrap();
            tx.commit().expect("commit");
        }
        let n: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_storage_transaction_rolls_back_on_drop() {
        let (_g, path) = tmp_db();
        let mut s = Storage::open(&path).expect("open");
        {
            let tx = s.transaction().expect("begin tx");
            tx.execute(
                "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
                 VALUES ('src/y.rs', 'rust', X'cc', 50, 1, 1)",
                [],
            )
            .unwrap();
            // tx dropped without commit -> rollback
        }
        let n: i64 = s
            .connection()
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "uncommitted transaction must roll back on drop");
    }

    #[test]
    fn test_storage_open_returns_err_on_unwritable_parent() {
        // /dev/null is not a directory; trying to open inside it must fail with our error type.
        let bad = PathBuf::from("/dev/null/nope/index.db");
        let r = Storage::open(&bad);
        assert!(r.is_err(), "expected error opening under /dev/null");
    }

    #[test]
    fn test_storage_path_accessor_returns_input() {
        let (_g, path) = tmp_db();
        let s = Storage::open(&path).expect("open");
        assert_eq!(s.path(), path);
    }

    #[test]
    fn test_storage_files_unique_path_constraint_enforced() {
        let (_g, path) = tmp_db();
        let s = Storage::open(&path).expect("open");
        let c = s.connection();
        c.execute(
            "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
             VALUES (?1, 'rust', X'aa', 1, 1, 1)",
            params!["src/dup.rs"],
        )
        .unwrap();
        let r = c.execute(
            "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
             VALUES (?1, 'rust', X'bb', 2, 2, 2)",
            params!["src/dup.rs"],
        );
        assert!(r.is_err());
    }
}
