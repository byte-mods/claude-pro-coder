use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LensError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("schema migration failed (from v{from} to v{to}): {message}")]
    Migration {
        from: u32,
        to: u32,
        message: String,
    },

    #[error("invalid path {path:?}: {reason}")]
    Path { path: PathBuf, reason: String },

    #[error("{0}")]
    Other(String),
}

impl LensError {
    pub fn io_at(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        LensError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        LensError::Other(message.into())
    }

    pub fn invalid_path(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        LensError::Path {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, LensError>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn test_error_io_includes_path_and_source() {
        let e = LensError::io_at(
            "/tmp/missing",
            std::io::Error::new(ErrorKind::NotFound, "no such file"),
        );
        let msg = e.to_string();
        assert!(msg.contains("/tmp/missing"), "msg missing path: {msg}");
        assert!(msg.contains("no such file"), "msg missing source: {msg}");
    }

    #[test]
    fn test_error_migration_displays_versions_and_message() {
        let e = LensError::Migration {
            from: 0,
            to: 1,
            message: "table missing".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("v0"), "msg missing from-version: {msg}");
        assert!(msg.contains("v1"), "msg missing to-version: {msg}");
        assert!(msg.contains("table missing"), "msg missing message: {msg}");
    }

    #[test]
    fn test_error_path_includes_path_and_reason() {
        let e = LensError::invalid_path("/etc/x", "non-utf8");
        let msg = e.to_string();
        assert!(msg.contains("/etc/x"), "msg missing path: {msg}");
        assert!(msg.contains("non-utf8"), "msg missing reason: {msg}");
    }

    #[test]
    fn test_error_other_preserves_message() {
        let e = LensError::other("boom");
        assert_eq!(e.to_string(), "boom");
    }

    #[test]
    fn test_result_alias_propagates_with_question_mark() {
        fn inner() -> Result<u32> {
            std::fs::read_to_string("/this/path/does/not/exist/lens-test").map_err(|e| {
                LensError::io_at("/this/path/does/not/exist/lens-test", e)
            })?;
            Ok(7)
        }
        let r = inner();
        assert!(r.is_err());
        match r.unwrap_err() {
            LensError::Io { path, .. } => assert_eq!(path.to_str(), Some("/this/path/does/not/exist/lens-test")),
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
