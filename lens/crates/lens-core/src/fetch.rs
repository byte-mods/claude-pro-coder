//! HTTP fetch for `lens add <url>` — graphify-parity for `graphify add`.
//!
//! Downloads `url` to `.lens/raw/<host>/<sha8>.<ext>`. Dedups by content
//! hash so re-adding the same URL is a no-op. Returns the saved path.
//!
//! ## Scope
//!
//! v1 supports HTTP(S) and `file://` URLs (the latter for local fixtures
//! and tests; deny-listed in production hardening). No HTML parsing, no
//! recursion, no credential handling. The downloaded blob is written
//! verbatim — if it's source code (`.py`, `.rs`, etc.), the caller can
//! feed it through the indexing pipeline.
//!
//! ## Storage layout
//!
//! ```text
//! .lens/
//! └── raw/
//!     └── <host>/
//!         ├── <sha8>.<ext>      ← the fetched blob
//!         └── <sha8>.meta       ← url + fetched_at + content_type
//! ```
//!
//! `<sha8>` is the first 8 hex chars of the blake3 hash of the response body.
//! Sufficient for human inspection; collisions are vanishingly unlikely at
//! a single project's scale.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{LensError, Result};

/// Outcome of a fetch operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    /// Absolute path of the saved blob.
    pub saved_to: PathBuf,
    /// True when the saved file already existed with the same content
    /// (no write happened).
    pub already_present: bool,
    /// Content length in bytes.
    pub bytes: u64,
    /// First 8 hex chars of the blake3 hash.
    pub sha8: String,
    /// Full 32-byte blake3 hash of the body — handy for callers (e.g.
    /// `cmd::add`) that need to feed the file into the indexing pipeline
    /// without re-reading + re-hashing.
    pub content_hash: [u8; 32],
}

/// Fetch `url` into `lens_root/raw/`, returning the saved path. `lens_root`
/// must exist; the function creates `raw/` and the host subdirectory as
/// needed.
///
/// Errors on:
/// - URL parse failure
/// - HTTP non-2xx status (the body is read first; ureq errors out on >=400)
/// - I/O failure when writing
pub fn fetch_to_raw(url: &str, lens_root: &Path) -> Result<FetchResult> {
    let parsed = url::Url::parse(url).map_err(|e| LensError::other(format!("parse url '{url}': {e}")))?;
    let host = parsed
        .host_str()
        .map(sanitise_host)
        .unwrap_or_else(|| "local".to_string());

    let raw_dir = lens_root.join("raw").join(&host);
    fs::create_dir_all(&raw_dir).map_err(|e| LensError::io_at(&raw_dir, e))?;

    // Download the body first so we can hash it before deciding the filename.
    let (body, content_type) = read_body(&parsed)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&body);
    let hash = hasher.finalize();
    let sha8 = hex8(&hash.as_bytes()[..4]);

    // Extension picked from URL path; falls back to .bin if none.
    let ext = parsed
        .path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|seg| seg.rsplit_once('.').map(|(_, e)| e.to_string()))
        .filter(|e| !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_string());

    let filename = format!("{sha8}.{ext}");
    let target = raw_dir.join(&filename);
    let meta_target = raw_dir.join(format!("{sha8}.meta"));

    let already_present = target.exists() && {
        let existing = fs::read(&target).map_err(|e| LensError::io_at(&target, e))?;
        existing == body
    };

    if !already_present {
        fs::write(&target, &body).map_err(|e| LensError::io_at(&target, e))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let meta = format!(
            "url={url}\nfetched_at={now}\ncontent_type={ct}\nbytes={n}\n",
            ct = content_type.unwrap_or_else(|| "unknown".to_string()),
            n = body.len(),
        );
        fs::write(&meta_target, meta).map_err(|e| LensError::io_at(&meta_target, e))?;
    }

    let mut content_hash = [0u8; 32];
    content_hash.copy_from_slice(hash.as_bytes());
    Ok(FetchResult {
        saved_to: target,
        already_present,
        bytes: body.len() as u64,
        sha8,
        content_hash,
    })
}

fn read_body(url: &url::Url) -> Result<(Vec<u8>, Option<String>)> {
    if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|_| LensError::other(format!("file url has no local path: {url}")))?;
        let body = fs::read(&path).map_err(|e| LensError::io_at(&path, e))?;
        return Ok((body, None));
    }

    let resp = ureq::get(url.as_str())
        .call()
        .map_err(|e| LensError::other(format!("fetch {url}: {e}")))?;
    let content_type = resp.header("content-type").map(String::from);
    let mut body = Vec::new();
    resp.into_reader()
        .take(64 * 1024 * 1024) // 64 MiB cap; refuse pathological responses
        .read_to_end(&mut body)
        .map_err(|e| LensError::other(format!("read body for {url}: {e}")))?;
    Ok((body, content_type))
}

/// Sanitise a host string for filesystem use. Keeps alnum + `.-_`, replaces
/// the rest with `_`. Empty input becomes `local`.
fn sanitise_host(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "local".into()
    } else {
        cleaned
    }
}

fn hex8(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn lens_root_in(dir: &TempDir) -> PathBuf {
        let p = dir.path().join(".lens");
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn test_fetch_file_url_writes_to_raw_dir() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_file = src_dir.path().join("payload.txt");
        fs::write(&src_file, b"hello world").unwrap();
        let url = format!("file://{}", src_file.display());

        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);

        let r = fetch_to_raw(&url, &lens_root).unwrap();
        assert!(r.saved_to.exists());
        assert!(!r.already_present);
        assert_eq!(r.bytes, b"hello world".len() as u64);
        let saved = fs::read(&r.saved_to).unwrap();
        assert_eq!(saved, b"hello world");
        assert!(r.saved_to.to_string_lossy().contains("/raw/"));
    }

    #[test]
    fn test_fetch_dedups_on_second_call_with_same_content() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_file = src_dir.path().join("payload.py");
        fs::write(&src_file, b"def x(): pass\n").unwrap();
        let url = format!("file://{}", src_file.display());

        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);

        let r1 = fetch_to_raw(&url, &lens_root).unwrap();
        assert!(!r1.already_present);
        let r2 = fetch_to_raw(&url, &lens_root).unwrap();
        assert!(r2.already_present, "second call must detect dedup");
        assert_eq!(r1.saved_to, r2.saved_to);
    }

    #[test]
    fn test_fetch_extension_inferred_from_url_path() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_file = src_dir.path().join("module.py");
        fs::write(&src_file, b"x = 1\n").unwrap();
        let url = format!("file://{}", src_file.display());

        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);

        let r = fetch_to_raw(&url, &lens_root).unwrap();
        assert!(
            r.saved_to.extension().map(|e| e == "py").unwrap_or(false),
            "expected .py extension; got {:?}",
            r.saved_to
        );
    }

    #[test]
    fn test_fetch_no_extension_falls_back_to_bin() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_file = src_dir.path().join("README");
        fs::write(&src_file, b"readme").unwrap();
        let url = format!("file://{}", src_file.display());

        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);

        let r = fetch_to_raw(&url, &lens_root).unwrap();
        assert_eq!(r.saved_to.extension().and_then(|s| s.to_str()), Some("bin"));
    }

    #[test]
    fn test_fetch_invalid_url_errors() {
        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);
        let r = fetch_to_raw("not a url", &lens_root);
        assert!(r.is_err());
    }

    #[test]
    fn test_fetch_meta_file_records_url_and_size() {
        let src_dir = tempfile::tempdir().unwrap();
        let src_file = src_dir.path().join("a.rs");
        let body = b"pub fn hi() {}\n";
        fs::write(&src_file, body).unwrap();
        let url = format!("file://{}", src_file.display());

        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);

        let r = fetch_to_raw(&url, &lens_root).unwrap();
        let meta_path = r.saved_to.with_extension("meta");
        let meta = fs::read_to_string(&meta_path).unwrap();
        assert!(meta.contains(&format!("url={url}")));
        assert!(meta.contains(&format!("bytes={}", body.len())));
    }

    #[test]
    fn test_sanitise_host_strips_non_alnum() {
        assert_eq!(sanitise_host("github.com"), "github.com");
        assert_eq!(sanitise_host("evil/path"), "evil_path");
        assert_eq!(sanitise_host(""), "local");
    }

    #[test]
    fn test_fetch_unsupported_scheme_errors() {
        let lens_dir = tempfile::tempdir().unwrap();
        let lens_root = lens_root_in(&lens_dir);
        let r = fetch_to_raw("ftp://example.com/x", &lens_root);
        assert!(r.is_err(), "unsupported schemes must error, not silently fail");
    }
}
