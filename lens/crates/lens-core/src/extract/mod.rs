//! Structured records produced by language extractors. Mirrors the SQLite
//! schema column shapes, minus surrogate IDs that are assigned at insert
//! time by the storage layer.

pub mod pipeline;
pub mod types;

pub use pipeline::{run, run_on_discovered};
pub use types::{
    ExtractContext, ExtractedCall, ExtractedFile, ExtractedImport, ExtractedRef, ExtractedSymbol,
    ExtractedTypeRel,
};
