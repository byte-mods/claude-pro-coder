//! Persistent index storage. SQLite-backed.

pub mod db;
pub mod diff;
pub mod insert;
pub mod migrations;
pub mod resolve;
pub mod resolve_heuristics;
pub mod update;

pub use db::Storage;
pub use diff::{apply_restamps, diff_against_index, load_file_stamps, FileDiff};
pub use insert::{insert_extracted_files, rebuild_extracted_files, InsertStats};
pub use migrations::{apply_migrations, current_schema_version, Migration, MIGRATIONS};
pub use resolve::{resolve_cross_file_references, ResolveStats};
pub use update::{update_files, UpdateStats};
