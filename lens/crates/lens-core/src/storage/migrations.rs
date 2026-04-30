use rusqlite::{params, Connection};

use crate::error::{LensError, Result};

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("schema.sql"),
    },
    Migration {
        // v2: add `doc_comment` column to symbols. Populated by language
        // extractors from `///`/`//!` (Rust), docstrings (Python), JSDoc
        // (TS/JS), and `//`/`/* */` preceding declarations (Go). Surfaced by
        // `lens follow` and `lens explain` so Claude can read intent without
        // dragging in body bytes.
        version: 2,
        sql: "ALTER TABLE symbols ADD COLUMN doc_comment TEXT;",
    },
];

pub fn current_schema_version(conn: &Connection) -> Result<u32> {
    let has_meta: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| LensError::other(format!("probe meta table: {e}")))?;
    if has_meta == 0 {
        return Ok(0);
    }
    let v: u32 = conn
        .query_row("SELECT schema_version FROM meta WHERE id = 0", [], |row| {
            row.get(0)
        })
        .map_err(|e| LensError::other(format!("read meta.schema_version: {e}")))?;
    Ok(v)
}

pub fn apply_migrations(conn: &Connection) -> Result<u32> {
    let mut current = current_schema_version(conn)?;
    let target = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
    if current >= target {
        return Ok(current);
    }
    for migration in MIGRATIONS {
        if migration.version <= current {
            continue;
        }
        run_one(conn, current, migration)?;
        current = migration.version;
    }
    Ok(current)
}

fn run_one(conn: &Connection, from: u32, migration: &Migration) -> Result<()> {
    let to = migration.version;
    let tx = conn.unchecked_transaction().map_err(|e| LensError::Migration {
        from,
        to,
        message: format!("begin transaction: {e}"),
    })?;
    tx.execute_batch(migration.sql).map_err(|e| LensError::Migration {
        from,
        to,
        message: format!("exec schema sql: {e}"),
    })?;
    tx.execute(
        "INSERT INTO meta (id, schema_version, lens_version) VALUES (0, ?1, ?2) \
         ON CONFLICT(id) DO UPDATE SET schema_version = excluded.schema_version, \
                                       lens_version = excluded.lens_version",
        params![to, env!("CARGO_PKG_VERSION")],
    )
    .map_err(|e| LensError::Migration {
        from,
        to,
        message: format!("update meta: {e}"),
    })?;
    tx.commit().map_err(|e| LensError::Migration {
        from,
        to,
        message: format!("commit: {e}"),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> Connection {
        Connection::open_in_memory().expect("open in-memory sqlite")
    }

    #[test]
    fn test_schema_v1_creates_all_tables() {
        let conn = open_in_memory();
        apply_migrations(&conn).expect("apply v1");
        for table in ["meta", "files", "symbols", "refs", "calls", "imports", "types"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} missing after v1 migration");
        }
    }

    #[test]
    fn test_schema_v1_creates_expected_indices() {
        let conn = open_in_memory();
        apply_migrations(&conn).expect("apply v1");
        let expected = [
            "idx_files_language",
            "idx_files_content_hash",
            "idx_symbols_qname_file",
            "idx_symbols_name",
            "idx_symbols_kind",
            "idx_symbols_qualified_name",
            "idx_symbols_parent",
            "idx_refs_symbol",
            "idx_refs_file",
            "idx_refs_raw_name",
            "idx_calls_caller",
            "idx_calls_callee",
            "idx_calls_callee_name",
            "idx_imports_file",
            "idx_imports_raw_path",
            "idx_types_symbol",
            "idx_types_target",
        ];
        for idx in expected {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    params![idx],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "index {idx} missing after v1 migration");
        }
    }

    #[test]
    fn test_apply_migrations_writes_meta_row_with_lens_version() {
        let conn = open_in_memory();
        let v = apply_migrations(&conn).expect("apply");
        let target = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
        assert_eq!(v, target);
        let stored_v: u32 = conn
            .query_row("SELECT schema_version FROM meta WHERE id = 0", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_v, target);
        let lens_v: String = conn
            .query_row("SELECT lens_version FROM meta WHERE id = 0", [], |row| row.get(0))
            .unwrap();
        assert_eq!(lens_v, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_apply_migrations_idempotent_on_already_migrated_db() {
        let conn = open_in_memory();
        let v1 = apply_migrations(&conn).expect("first");
        let v2 = apply_migrations(&conn).expect("second");
        assert_eq!(v1, v2);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 1, "re-applying must not duplicate meta rows");
    }

    #[test]
    fn test_current_schema_version_returns_zero_on_empty_db() {
        let conn = open_in_memory();
        let v = current_schema_version(&conn).expect("query");
        assert_eq!(v, 0);
    }

    #[test]
    fn test_meta_table_rejects_non_zero_id() {
        let conn = open_in_memory();
        apply_migrations(&conn).expect("apply");
        let r = conn.execute(
            "INSERT INTO meta (id, schema_version, lens_version) VALUES (1, 1, 'x')",
            [],
        );
        assert!(r.is_err(), "expected CHECK (id = 0) to reject row with id=1");
    }

    #[test]
    fn test_files_path_is_unique() {
        let conn = open_in_memory();
        apply_migrations(&conn).expect("apply");
        let now = 1700000000_i64;
        conn.execute(
            "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params!["src/a.rs", "rust", b"hash"[..].to_vec(), 100i64, now, now],
        )
        .unwrap();
        let r = conn.execute(
            "INSERT INTO files (path, language, content_hash, size_bytes, modified_at, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params!["src/a.rs", "rust", b"otherhash"[..].to_vec(), 200i64, now, now],
        );
        assert!(r.is_err(), "expected UNIQUE constraint on files.path");
    }

    #[test]
    fn test_migration_error_carries_versions() {
        // Create a connection where meta exists but schema_version is way ahead.
        // apply_migrations should be a no-op for a future-versioned DB (current >= target).
        let conn = open_in_memory();
        apply_migrations(&conn).expect("v1");
        conn.execute("UPDATE meta SET schema_version = 99 WHERE id = 0", []).unwrap();
        let v = apply_migrations(&conn).expect("future-versioned db is not an error");
        assert_eq!(v, 99);
    }
}
