//! Project filesystem walker. Discovers files whose extension maps to a
//! registered language, hashes their contents with blake3, and returns
//! `DiscoveredFile` records ready for the parse + extract pipeline.

pub mod discover;

pub use discover::{discover, DiscoveredFile};
