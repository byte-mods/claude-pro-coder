//! Persistent index storage. SQLite-backed.

pub mod db;
pub mod diff;
pub mod insert;
pub mod migrations;
pub mod resolve;
pub mod update;

pub use db::Storage;
pub use diff::{diff_against_index, FileDiff};
pub use insert::{insert_extracted_files, InsertStats};
pub use migrations::{apply_migrations, current_schema_version, Migration, MIGRATIONS};
pub use resolve::{resolve_cross_file_references, ResolveStats};
pub use update::{update_files, UpdateStats};
