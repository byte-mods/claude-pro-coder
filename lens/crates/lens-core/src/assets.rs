//! Asset layer — images, video, audio, PDF, Office documents, archives and
//! other binary files.
//!
//! The text index (`docs.rs`) covers anything a model could read as text.
//! Everything else used to be skipped as "binary", which meant a model had
//! to open a PDF or view an image every single time it needed to know what
//! was in it. This module gives every binary file a `docs` row (kind =
//! `image` / `video` / `audio` / `pdf` / `office` / `archive` / `binary`)
//! whose searchable body is assembled from three sources, in order of
//! trust:
//!
//! 1. **Extracted text** — what lens can pull out on its own, with no
//!    external tools and no model: PDF page count and text (FlateDecode
//!    content streams, `Tj`/`TJ`/`'`/`"` operators), Office XML text
//!    (`.docx` / `.xlsx` / `.pptx` / `.odt` / `.odp` / `.ods` are zip
//!    archives of XML — a minimal zip reader + tag stripper), image
//!    dimensions from PNG/JPEG/GIF/WebP/BMP headers, MP4/MOV duration from
//!    the `mvhd` atom, WAV duration from the format chunk.
//! 2. **External extractors, when present** — `pdftotext` (poppler) for
//!    PDFs and `ffprobe` (ffmpeg) for media durations produce better data
//!    than the built-ins; they are used opportunistically and never
//!    required. `LENS_NO_EXTERNAL_TOOLS=1` disables them.
//! 3. **A stored description** — `lens describe <file> --text "…"` lets a
//!    model (Claude viewing an image, a user, another agent) record what
//!    the file *is*. The description survives re-indexing of the file
//!    (keyed by path, carried over when the bytes change) and is part of
//!    the search body, so the next lookup — by any model, in any session —
//!    is a `lens search` hit instead of a re-read. That is the token loop
//!    for media: view once, describe once, search forever.
//!
//! Large files are never read whole: metadata parsers read a bounded
//! prefix, and identity for change detection is `blake3(prefix || size)`.
//! PDF / Office text extraction reads the file only up to
//! [`MAX_EXTRACT_BYTES`].

use std::io::Read;
use std::path::Path;
use std::process::Command;

use crate::tokens::estimate_tokens;

/// Bytes read for header-based metadata (dimensions, durations).
pub const HEADER_PROBE_BYTES: usize = 1024 * 1024;

/// Upper bound for whole-file text extraction (PDF, Office). Above this
/// only metadata is recorded.
pub const MAX_EXTRACT_BYTES: u64 = 16 * 1024 * 1024;

/// Cap on extracted text kept per asset (keeps the FTS body and the
/// per-file token estimate sane for 500-page manuals).
pub const MAX_EXTRACTED_CHARS: usize = 200_000;

/// Metadata extracted for one asset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetMeta {
    /// `image` | `video` | `audio` | `pdf` | `office` | `archive` | `binary`.
    pub kind: &'static str,
    /// Best-effort MIME-ish label (`image/png`, `video/mp4`, `application/pdf`).
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub pages: Option<u32>,
    /// Text pulled out of the file (PDF / Office), already capped.
    pub extracted_text: String,
    /// Which extractor produced `extracted_text` (`builtin`, `pdftotext`, …).
    pub extractor: &'static str,
}

impl AssetMeta {
    /// One-line human summary, e.g. `[image 1024x768 image/png]`.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = vec![self.kind.to_string()];
        if let (Some(w), Some(h)) = (self.width, self.height) {
            parts.push(format!("{w}x{h}"));
        }
        if let Some(d) = self.duration_ms {
            parts.push(format!("{:.1}s", d as f64 / 1000.0));
        }
        if let Some(p) = self.pages {
            parts.push(format!("{p} page{}", if p == 1 { "" } else { "s" }));
        }
        if !self.mime.is_empty() {
            parts.push(self.mime.clone());
        }
        format!("[{}]", parts.join(" "))
    }
}

/// Build the searchable FTS body for an asset: summary line, description
/// (if any), extracted text.
pub fn build_body(meta: &AssetMeta, description: Option<&str>) -> String {
    let mut body = meta.summary();
    body.push('\n');
    if let Some(d) = description.filter(|d| !d.trim().is_empty()) {
        body.push_str("description: ");
        body.push_str(d.trim());
        body.push('\n');
    }
    if !meta.extracted_text.is_empty() {
        body.push_str(&meta.extracted_text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    body
}

/// Token cost of a body — what a `lens search` hit or `lens describe`
/// read costs, used for the docs row's `token_estimate`.
pub fn body_tokens(body: &str) -> u32 {
    estimate_tokens(body)
}

/// Classify by extension. `binary` for anything unknown.
pub fn kind_for_extension(ext: &str) -> (&'static str, &'static str) {
    match ext.to_ascii_lowercase().as_str() {
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "gif" => ("image", "image/gif"),
        "webp" => ("image", "image/webp"),
        "bmp" => ("image", "image/bmp"),
        "ico" => ("image", "image/x-icon"),
        "tif" | "tiff" => ("image", "image/tiff"),
        "heic" | "heif" => ("image", "image/heic"),
        "psd" => ("image", "image/vnd.adobe.photoshop"),
        "mp4" | "m4v" => ("video", "video/mp4"),
        "mov" => ("video", "video/quicktime"),
        "mkv" => ("video", "video/x-matroska"),
        "webm" => ("video", "video/webm"),
        "avi" => ("video", "video/x-msvideo"),
        "mp3" => ("audio", "audio/mpeg"),
        "wav" => ("audio", "audio/wav"),
        "flac" => ("audio", "audio/flac"),
        "ogg" | "oga" => ("audio", "audio/ogg"),
        "m4a" | "aac" => ("audio", "audio/mp4"),
        "pdf" => ("pdf", "application/pdf"),
        "docx" => ("office", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => ("office", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "pptx" => ("office", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "odt" => ("office", "application/vnd.oasis.opendocument.text"),
        "ods" => ("office", "application/vnd.oasis.opendocument.spreadsheet"),
        "odp" => ("office", "application/vnd.oasis.opendocument.presentation"),
        "zip" | "jar" | "whl" => ("archive", "application/zip"),
        "tar" | "tgz" | "gz" | "bz2" | "xz" | "7z" | "rar" => ("archive", "application/x-archive"),
        "woff" | "woff2" | "ttf" | "otf" => ("binary", "font/*"),
        _ => ("binary", "application/octet-stream"),
    }
}

/// Extract metadata (and text where possible) for the file at `abs`.
/// `size_bytes` is used to decide whether whole-file extraction is allowed.
/// Never fails: an unreadable or malformed file yields a bare `binary`
/// record so the path is still searchable by name.
pub fn extract(abs: &Path, size_bytes: u64) -> AssetMeta {
    let ext = abs.extension().and_then(|e| e.to_str()).unwrap_or("");
    let (kind, mime) = kind_for_extension(ext);
    let mut meta = AssetMeta { kind, mime: mime.to_string(), extractor: "none", ..Default::default() };

    let head = read_prefix(abs, HEADER_PROBE_BYTES);
    match kind {
        "image" => {
            if let Some((w, h, m)) = image_dimensions(&head) {
                meta.width = Some(w);
                meta.height = Some(h);
                if !m.is_empty() {
                    meta.mime = m.to_string();
                }
            }
        }
        "video" | "audio" => {
            meta.duration_ms = external_duration_ms(abs)
                .or_else(|| mp4_duration_ms(&head))
                .or_else(|| wav_duration_ms(&head));
        }
        "pdf" => {
            if size_bytes <= MAX_EXTRACT_BYTES {
                let bytes = std::fs::read(abs).unwrap_or_default();
                meta.pages = pdf_page_count(&bytes);
                if let Some(text) = external_pdf_text(abs) {
                    meta.extracted_text = cap(text);
                    meta.extractor = "pdftotext";
                } else {
                    meta.extracted_text = cap(pdf_text(&bytes));
                    meta.extractor = "builtin";
                }
            } else {
                meta.pages = pdf_page_count(&head);
            }
        }
        "office" => {
            if size_bytes <= MAX_EXTRACT_BYTES {
                let bytes = std::fs::read(abs).unwrap_or_default();
                meta.extracted_text = cap(office_text(&bytes, ext));
                meta.extractor = "builtin";
            }
        }
        _ => {}
    }
    meta
}

fn cap(s: String) -> String {
    if s.chars().count() <= MAX_EXTRACTED_CHARS {
        return s;
    }
    let mut out: String = s.chars().take(MAX_EXTRACTED_CHARS).collect();
    out.push_str("\n[… truncated]");
    out
}

fn read_prefix(abs: &Path, n: usize) -> Vec<u8> {
    let Ok(f) = std::fs::File::open(abs) else { return Vec::new() };
    let mut buf = Vec::with_capacity(n.min(64 * 1024));
    let _ = f.take(n as u64).read_to_end(&mut buf);
    buf
}

fn external_tools_disabled() -> bool {
    matches!(std::env::var("LENS_NO_EXTERNAL_TOOLS").as_deref(), Ok("1") | Ok("true"))
}

/// `pdftotext -layout FILE -` when poppler is installed.
fn external_pdf_text(abs: &Path) -> Option<String> {
    if external_tools_disabled() {
        return None;
    }
    let out = Command::new("pdftotext").arg("-layout").arg(abs).arg("-").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `ffprobe` duration when ffmpeg is installed.
fn external_duration_ms(abs: &Path) -> Option<u64> {
    if external_tools_disabled() {
        return None;
    }
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(abs)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let secs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some((secs * 1000.0) as u64)
}

// ----- Image headers ---------------------------------------------------------

/// `(width, height, mime)` from the first bytes of PNG / JPEG / GIF / WebP / BMP.
pub fn image_dimensions(b: &[u8]) -> Option<(u32, u32, &'static str)> {
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        return Some((w, h, "image/png"));
    }
    if b.len() >= 10 && b.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([b[6], b[7]]) as u32;
        let h = u16::from_le_bytes([b[8], b[9]]) as u32;
        return Some((w, h, "image/gif"));
    }
    if b.len() >= 26 && b.starts_with(b"BM") {
        let w = i32::from_le_bytes([b[18], b[19], b[20], b[21]]).unsigned_abs();
        let h = i32::from_le_bytes([b[22], b[23], b[24], b[25]]).unsigned_abs();
        return Some((w, h, "image/bmp"));
    }
    if b.len() >= 30 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        match &b[12..16] {
            b"VP8 " if b.len() >= 30 => {
                let w = (u16::from_le_bytes([b[26], b[27]]) & 0x3fff) as u32;
                let h = (u16::from_le_bytes([b[28], b[29]]) & 0x3fff) as u32;
                return Some((w, h, "image/webp"));
            }
            b"VP8L" if b.len() >= 25 => {
                let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
                let w = (bits & 0x3fff) + 1;
                let h = ((bits >> 14) & 0x3fff) + 1;
                return Some((w, h, "image/webp"));
            }
            b"VP8X" if b.len() >= 30 => {
                let w = (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16) + 1;
                let h = (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16) + 1;
                return Some((w, h, "image/webp"));
            }
            _ => return None,
        }
    }
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        // JPEG: walk segments to the first SOFn marker.
        let mut i = 2usize;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
            if is_sof {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((w, h, "image/jpeg"));
            }
            if marker == 0xD9 || marker == 0xDA {
                break;
            }
            i += 2 + len;
        }
    }
    None
}

// ----- Media headers ----------------------------------------------------------

/// MP4 / MOV duration from the `moov/mvhd` atom, when it sits in the probed
/// prefix (it does for files written with `-movflags faststart`; otherwise
/// `ffprobe`, if present, covers it).
pub fn mp4_duration_ms(b: &[u8]) -> Option<u64> {
    let pos = find(b, b"mvhd")?;
    // `mvhd` box: 4 size + 4 type ("mvhd" at `pos`) + 1 version + 3 flags.
    let v = *b.get(pos + 4)?;
    let (timescale, duration) = if v == 1 {
        // creation(8) modification(8) timescale(4) duration(8)
        let ts_at = pos + 8 + 16;
        let ts = u32::from_be_bytes(b.get(ts_at..ts_at + 4)?.try_into().ok()?);
        let du = u64::from_be_bytes(b.get(ts_at + 4..ts_at + 12)?.try_into().ok()?);
        (ts, du)
    } else {
        let ts_at = pos + 8 + 8;
        let ts = u32::from_be_bytes(b.get(ts_at..ts_at + 4)?.try_into().ok()?);
        let du = u32::from_be_bytes(b.get(ts_at + 4..ts_at + 8)?.try_into().ok()?) as u64;
        (ts, du)
    };
    if timescale == 0 {
        return None;
    }
    Some(duration.saturating_mul(1000) / timescale as u64)
}

/// WAV duration from the `fmt ` chunk byte rate and the `data` chunk size.
pub fn wav_duration_ms(b: &[u8]) -> Option<u64> {
    if !(b.starts_with(b"RIFF") && b.get(8..12)? == b"WAVE") {
        return None;
    }
    let fmt = find(b, b"fmt ")?;
    let byte_rate = u32::from_le_bytes(b.get(fmt + 16..fmt + 20)?.try_into().ok()?);
    let data = find(b, b"data")?;
    let data_len = u32::from_le_bytes(b.get(data + 4..data + 8)?.try_into().ok()?);
    if byte_rate == 0 {
        return None;
    }
    Some(data_len as u64 * 1000 / byte_rate as u64)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ----- PDF -------------------------------------------------------------------

/// Number of `/Type /Page` objects (excluding `/Pages` nodes).
pub fn pdf_page_count(b: &[u8]) -> Option<u32> {
    let mut count = 0u32;
    let mut i = 0usize;
    while let Some(p) = find(&b[i..], b"/Type") {
        let at = i + p + 5;
        let rest = &b[at..b.len().min(at + 16)];
        let s: String = rest.iter().map(|c| *c as char).collect();
        let t = s.trim_start();
        if t.starts_with("/Page") && !t.starts_with("/Pages") {
            count += 1;
        }
        i = at;
    }
    if count == 0 {
        None
    } else {
        Some(count)
    }
}

/// Best-effort text from PDF content streams: inflate every FlateDecode
/// stream (raw streams are taken as-is), then collect string operands of
/// the text-showing operators. Fonts with custom encodings (CID/Identity-H)
/// come out as gibberish or nothing — `pdftotext`, when installed, is used
/// in preference.
pub fn pdf_text(b: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(p) = find(&b[i..], b"stream") {
        let start = i + p + 6;
        // Skip EOL after `stream`.
        let mut s = start;
        if b.get(s) == Some(&b'\r') {
            s += 1;
        }
        if b.get(s) == Some(&b'\n') {
            s += 1;
        }
        let Some(e) = find(&b[s..], b"endstream") else { break };
        let raw = &b[s..s + e];
        // Is this stream FlateDecode? Look back at the dictionary.
        let dict_start = b[..start].iter().rposition(|c| *c == b'<').unwrap_or(0).saturating_sub(64);
        let dict: String = b[dict_start..start].iter().map(|c| *c as char).collect();
        let data: Vec<u8> = if dict.contains("FlateDecode") {
            let mut d = flate2::read::ZlibDecoder::new(raw);
            let mut v = Vec::new();
            if d.read_to_end(&mut v).is_err() && v.is_empty() {
                i = s + e + 9;
                continue;
            }
            v
        } else if dict.contains("/Filter") {
            i = s + e + 9;
            continue; // other filters (DCT, LZW, …) — not text.
        } else {
            raw.to_vec()
        };
        let piece = text_ops(&data);
        if !piece.trim().is_empty() {
            out.push_str(&piece);
            out.push('\n');
        }
        i = s + e + 9;
    }
    normalise_ws(&out)
}

/// Collect `(…) Tj`, `[…] TJ`, `(…) '`, `(…) "` operands and `<hex>` strings
/// from a content stream; `T*`, `'`, `"` and `TJ` gaps become breaks.
fn text_ops(data: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    let n = data.len();
    let mut pending: Vec<String> = Vec::new();
    while i < n {
        match data[i] {
            b'(' => {
                let (s, next) = read_literal(data, i);
                pending.push(s);
                i = next;
            }
            b'<' if data.get(i + 1) != Some(&b'<') => {
                let (s, next) = read_hex(data, i);
                pending.push(s);
                i = next;
            }
            b'T' | b'\'' | b'"' => {
                let op2 = data.get(i + 1).copied();
                let is_show = (data[i] == b'T' && matches!(op2, Some(b'j') | Some(b'J')))
                    || data[i] == b'\''
                    || data[i] == b'"';
                let is_newline = (data[i] == b'T' && matches!(op2, Some(b'*') | Some(b'd') | Some(b'D')))
                    || data[i] == b'\''
                    || data[i] == b'"';
                if is_show {
                    out.push_str(&pending.join(""));
                    pending.clear();
                    out.push(' ');
                }
                if is_newline && !out.ends_with('\n') {
                    out.push('\n');
                }
                if data[i] == b'T' && matches!(op2, Some(b'j') | Some(b'J') | Some(b'*') | Some(b'd') | Some(b'D')) {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            b'E' if data.get(i + 1) == Some(&b'T') => {
                pending.clear();
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

fn read_literal(data: &[u8], start: usize) -> (String, usize) {
    let mut depth = 0i32;
    let mut i = start;
    let mut s = String::new();
    while i < data.len() {
        let c = data[i];
        match c {
            b'\\' => {
                if let Some(&nx) = data.get(i + 1) {
                    match nx {
                        b'n' => s.push('\n'),
                        b'r' => {}
                        b't' => s.push('\t'),
                        b'(' | b')' | b'\\' => s.push(nx as char),
                        b'0'..=b'7' => {
                            let mut v = 0u32;
                            let mut k = 0;
                            while k < 3 && i + 1 + k < data.len() && (b'0'..=b'7').contains(&data[i + 1 + k]) {
                                v = v * 8 + (data[i + 1 + k] - b'0') as u32;
                                k += 1;
                            }
                            if let Some(ch) = char::from_u32(v) {
                                s.push(ch);
                            }
                            i += k - 1;
                        }
                        _ => {}
                    }
                    i += 2;
                    continue;
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                if depth > 1 {
                    s.push('(');
                }
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return (s, i);
                }
                s.push(')');
            }
            _ => {
                if c.is_ascii() {
                    s.push(c as char);
                } else {
                    s.push(char::from(c));
                }
                i += 1;
            }
        }
    }
    (s, i)
}

fn read_hex(data: &[u8], start: usize) -> (String, usize) {
    let mut i = start + 1;
    let mut hex = String::new();
    while i < data.len() && data[i] != b'>' {
        if data[i].is_ascii_hexdigit() {
            hex.push(data[i] as char);
        }
        i += 1;
    }
    let mut s = String::new();
    let bytes: Vec<u8> = hex
        .as_bytes()
        .chunks(2)
        .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect();
    // Two-byte (Identity-H) strings render as their low bytes; single-byte
    // strings as ASCII. Non-printables are dropped either way.
    for b in bytes {
        if b.is_ascii_graphic() || b == b' ' {
            s.push(b as char);
        }
    }
    (s, i + 1)
}

fn normalise_ws(s: &str) -> String {
    let mut out = String::new();
    for line in s.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if !t.is_empty() {
            out.push_str(&t.join(" "));
            out.push('\n');
        }
    }
    out
}

// ----- Office (zip + XML) ------------------------------------------------------

/// Text from OOXML / ODF packages. Which XML parts are read depends on the
/// extension; unknown parts are ignored.
pub fn office_text(bytes: &[u8], ext: &str) -> String {
    let wanted: &[&str] = match ext.to_ascii_lowercase().as_str() {
        "docx" => &["word/document.xml", "word/footnotes.xml", "word/endnotes.xml"],
        "xlsx" => &["xl/sharedStrings.xml"],
        "pptx" => &["ppt/slides/"],
        "odt" | "ods" | "odp" => &["content.xml"],
        _ => &[],
    };
    let mut out = String::new();
    for (name, data) in zip_entries(bytes) {
        let hit = wanted.iter().any(|w| if w.ends_with('/') { name.starts_with(w) && name.ends_with(".xml") } else { name == *w });
        if !hit {
            continue;
        }
        let xml = String::from_utf8_lossy(&data);
        let text = strip_xml(&xml);
        if !text.trim().is_empty() {
            if wanted.len() > 1 || wanted[0].ends_with('/') {
                out.push_str(&format!("[{name}]\n"));
            }
            out.push_str(&text);
            out.push('\n');
        }
    }
    normalise_ws(&out)
}

/// Minimal ZIP reader: central directory → local headers → stored or
/// deflated payload. ZIP64 and encryption are not supported (entries are
/// skipped). Returns `(name, bytes)` pairs.
pub fn zip_entries(b: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let tail_start = b.len().saturating_sub(66 * 1024);
    let Some(eocd_rel) = b[tail_start..].windows(4).rposition(|w| w == [0x50, 0x4b, 0x05, 0x06]) else {
        return out;
    };
    let eocd = tail_start + eocd_rel;
    let Some(cd_off) = b.get(eocd + 16..eocd + 20).map(|x| u32::from_le_bytes(x.try_into().unwrap_or([0; 4])) as usize) else {
        return out;
    };
    let mut p = cd_off;
    while p + 46 <= b.len() && b[p..p + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let method = u16::from_le_bytes([b[p + 10], b[p + 11]]);
        let csize = u32::from_le_bytes(b[p + 20..p + 24].try_into().unwrap_or([0; 4])) as usize;
        let nlen = u16::from_le_bytes([b[p + 28], b[p + 29]]) as usize;
        let xlen = u16::from_le_bytes([b[p + 30], b[p + 31]]) as usize;
        let clen = u16::from_le_bytes([b[p + 32], b[p + 33]]) as usize;
        let lho = u32::from_le_bytes(b[p + 42..p + 46].try_into().unwrap_or([0; 4])) as usize;
        let name = String::from_utf8_lossy(&b[p + 46..(p + 46 + nlen).min(b.len())]).to_string();
        p += 46 + nlen + xlen + clen;
        if lho + 30 > b.len() || b[lho..lho + 4] != [0x50, 0x4b, 0x03, 0x04] {
            continue;
        }
        let lnlen = u16::from_le_bytes([b[lho + 26], b[lho + 27]]) as usize;
        let lxlen = u16::from_le_bytes([b[lho + 28], b[lho + 29]]) as usize;
        let data_start = lho + 30 + lnlen + lxlen;
        let Some(raw) = b.get(data_start..data_start + csize) else { continue };
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut d = flate2::read::DeflateDecoder::new(raw);
                let mut v = Vec::new();
                if d.read_to_end(&mut v).is_err() {
                    continue;
                }
                v
            }
            _ => continue,
        };
        out.push((name, data));
    }
    out
}

/// Drop tags, decode the five XML entities, and turn paragraph / row /
/// cell boundaries into whitespace.
pub fn strip_xml(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for c in xml.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let t = tag.trim_start_matches('/');
                let name = t.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
                match name {
                    "w:p" | "text:p" | "text:h" | "a:p" | "row" | "table:table-row" | "w:br" | "text:line-break" => out.push('\n'),
                    "w:tab" | "text:tab" | "c" | "table:table-cell" | "text:s" => out.push(' '),
                    _ => {}
                }
            }
            _ if in_tag => tag.push(c),
            _ => out.push(c),
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn test_assets_image_dimensions_png_gif_bmp_jpeg_webp() {
        assert_eq!(image_dimensions(&png(1024, 768)), Some((1024, 768, "image/png")));
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xF0, 0x00]);
        assert_eq!(image_dimensions(&gif), Some((320, 240, "image/gif")));
        let mut bmp = vec![0u8; 26];
        bmp[0] = b'B';
        bmp[1] = b'M';
        bmp[18..22].copy_from_slice(&64i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-32i32).to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((64, 32, "image/bmp")));
        // JPEG: SOI, APP0 (len 16), SOF0 with 480x640.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpg.extend_from_slice(&[0u8; 14]);
        jpg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80, 0x03]);
        assert_eq!(image_dimensions(&jpg), Some((640, 480, "image/jpeg")));
        // WebP VP8L: 1-bit signature then 14-bit w-1, 14-bit h-1.
        let mut webp = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        let bits: u32 = (99u32) | (49u32 << 14);
        webp.extend_from_slice(&bits.to_le_bytes());
        webp.extend_from_slice(&[0u8; 8]);
        assert_eq!(image_dimensions(&webp), Some((100, 50, "image/webp")));
        assert_eq!(image_dimensions(b"not an image"), None);
    }

    #[test]
    fn test_assets_mp4_and_wav_duration() {
        // mvhd v0: size,type, version+flags, ctime, mtime, timescale=1000, duration=90500
        let mut mp4 = b"\0\0\0\x08ftypisom".to_vec();
        mp4.extend_from_slice(&[0, 0, 0, 108]);
        mp4.extend_from_slice(b"mvhd");
        mp4.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        mp4.extend_from_slice(&1000u32.to_be_bytes());
        mp4.extend_from_slice(&90500u32.to_be_bytes());
        assert_eq!(mp4_duration_ms(&mp4), Some(90500));

        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
        wav.extend_from_slice(&[2, 0, 16, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&48000u32.to_le_bytes());
        assert_eq!(wav_duration_ms(&wav), Some(3000));
    }

    fn make_pdf(text_lines: &[&str]) -> Vec<u8> {
        let content: String = text_lines.iter().map(|l| format!("BT /F1 12 Tf ({l}) Tj ET\n")).collect();
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(content.as_bytes()).unwrap();
        let compressed = enc.finish().unwrap();
        let mut pdf = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /Contents 4 0 R >> endobj\n".to_vec();
        pdf.extend_from_slice(format!("4 0 obj << /Length {} /Filter /FlateDecode >>\nstream\n", compressed.len()).as_bytes());
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        pdf
    }

    #[test]
    fn test_assets_pdf_page_count_and_flate_text() {
        let pdf = make_pdf(&["Hello lens", "Second line (with parens)"]);
        assert_eq!(pdf_page_count(&pdf), Some(1));
        let text = pdf_text(&pdf);
        assert!(text.contains("Hello lens"), "{text}");
        assert!(text.contains("Second line"), "{text}");
    }

    #[test]
    fn test_assets_pdf_raw_stream_and_hex_strings() {
        let pdf = b"%PDF-1.4\n<< /Type /Page >>\n4 0 obj << /Length 40 >>\nstream\nBT [(Ab) -20 (cd)] TJ <48656C6C6F> Tj ET\nendstream\n".to_vec();
        let text = pdf_text(&pdf);
        assert!(text.contains("Abcd"), "{text}");
        assert!(text.contains("Hello"), "{text}");
    }

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        // Stored (method 0) entries — enough to exercise the reader.
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data) in entries {
            let lho = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend_from_slice(&lho.to_le_bytes());
            cd.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn test_assets_docx_text_from_zip_xml() {
        let doc = br#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Quarterly &amp; annual</w:t></w:r></w:p><w:p><w:r><w:t>Second para</w:t></w:r></w:p></w:body></w:document>"#;
        let z = make_zip(&[("[Content_Types].xml", b"<Types/>"), ("word/document.xml", doc)]);
        let text = office_text(&z, "docx");
        assert!(text.contains("Quarterly & annual"), "{text}");
        assert!(text.contains("Second para"), "{text}");
        assert!(!text.contains("<w:"), "tags stripped: {text}");
    }

    #[test]
    fn test_assets_xlsx_shared_strings_and_pptx_slides() {
        let ss = br#"<sst><si><t>Revenue</t></si><si><t>Cost</t></si></sst>"#;
        let z = make_zip(&[("xl/sharedStrings.xml", ss)]);
        let t = office_text(&z, "xlsx");
        assert!(t.contains("Revenue") && t.contains("Cost"), "{t}");
        let slide = br#"<p:sld><a:p><a:r><a:t>Roadmap 2027</a:t></a:r></a:p></p:sld>"#;
        let z = make_zip(&[("ppt/slides/slide1.xml", slide), ("ppt/presentation.xml", b"<x/>")]);
        let t = office_text(&z, "pptx");
        assert!(t.contains("[ppt/slides/slide1.xml]") && t.contains("Roadmap 2027"), "{t}");
    }

    #[test]
    fn test_assets_extract_end_to_end_on_disk_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("logo.png");
        std::fs::write(&p, png(300, 200)).unwrap();
        std::env::set_var("LENS_NO_EXTERNAL_TOOLS", "1");
        let m = extract(&p, 100);
        std::env::remove_var("LENS_NO_EXTERNAL_TOOLS");
        assert_eq!(m.kind, "image");
        assert_eq!((m.width, m.height), (Some(300), Some(200)));
        assert_eq!(m.summary(), "[image 300x200 image/png]");

        let pdf_path = dir.path().join("spec.pdf");
        std::fs::write(&pdf_path, make_pdf(&["Acceptance criteria"])).unwrap();
        let m = extract(&pdf_path, 500);
        assert_eq!(m.kind, "pdf");
        assert_eq!(m.pages, Some(1));
        assert!(m.extracted_text.contains("Acceptance criteria"));
        let body = build_body(&m, Some("The product spec"));
        assert!(body.starts_with("[pdf 1 page application/pdf]\ndescription: The product spec\n"), "{body}");
        assert!(body.contains("Acceptance criteria"));

        let bin = dir.path().join("blob.bin");
        std::fs::write(&bin, [0u8, 1, 2]).unwrap();
        let m = extract(&bin, 3);
        assert_eq!(m.kind, "binary");
        assert_eq!(m.summary(), "[binary application/octet-stream]");
    }

    #[test]
    fn test_assets_strip_xml_entities_and_breaks() {
        let s = normalise_ws(&strip_xml("<a><w:p>x &lt;y&gt; &amp; z</w:p><w:p>next</w:p></a>"));
        assert_eq!(s, "x <y> & z\nnext\n");
    }

    #[test]
    fn test_assets_kind_for_extension_is_case_insensitive() {
        assert_eq!(kind_for_extension("PNG").0, "image");
        assert_eq!(kind_for_extension("Mp4").0, "video");
        assert_eq!(kind_for_extension("xyz").0, "binary");
    }
}
