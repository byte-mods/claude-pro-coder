//! `lens describe <file>` — show what lens knows about a binary asset
//! (image / video / audio / PDF / Office / archive), or store what a model
//! learned by looking at it.
//!
//! View once, describe once, search forever: after `lens describe img.png
//! --text "…"`, every later question about that image is a `lens search`
//! hit for any model in any session instead of a re-read of the bytes.

use std::path::Path;

use lens_core::{describe_asset, estimate_tokens, set_asset_description, AssetRecord};

pub fn run(path: &Path, text: Option<&str>, by: Option<&str>, clear: bool, budget: u32) -> Result<(), u8> {
    let cwd = match crate::cmd::util::cwd_project_root("describe") {
        Ok(p) => p,
        Err(code) => return Err(code),
    };
    run_with_root(&cwd, path, text, by, clear, budget)
}

pub fn run_with_root(
    root: &Path,
    path: &Path,
    text: Option<&str>,
    by: Option<&str>,
    clear: bool,
    budget: u32,
) -> Result<(), u8> {
    if clear && text.is_some() {
        eprintln!("lens describe: --clear and --text are mutually exclusive.");
        return Err(2);
    }
    let (mut storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "describe") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let Some(rel) = lens_core::docs::scope_to_relative(root, path) else {
        eprintln!("lens describe: '{}' is not under project root '{}'", path.display(), root.display());
        return Err(1);
    };

    if clear || text.is_some() {
        let desc = if clear { None } else { text };
        match set_asset_description(&mut storage, &rel, desc, by.or(Some("user"))) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "lens describe: '{rel}' is not an indexed asset (text and code files are searchable as-is; \
                     binary files appear after `lens update`)."
                );
                return Err(1);
            }
            Err(e) => {
                eprintln!("lens describe: write failed: {e}");
                return Err(1);
            }
        }
    }

    let rec = match describe_asset(&storage, &rel) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "lens describe: '{rel}' is not an indexed asset. Text and code files do not need describing — \
                 use `lens search` / `lens slice`. Binary files appear after `lens update`."
            );
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens describe: lookup failed: {e}");
            return Err(1);
        }
    };
    let rendered = render_markdown(&rec, budget);
    crate::cmd::util::finish(root, &storage, rendered, &[]);
    Ok(())
}

/// Pure markdown formatter. `budget` caps the extracted-text excerpt.
pub fn render_markdown(rec: &AssetRecord, budget: u32) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Describe: `{}`", rec.path);
    let _ = writeln!(&mut out);
    let mut facts: Vec<String> = vec![format!("**Kind:** {}", rec.kind), format!("**Type:** {}", rec.mime), format!("**Size:** {}", human_size(rec.size_bytes))];
    if let (Some(w), Some(h)) = (rec.width, rec.height) {
        facts.push(format!("**Dimensions:** {w}×{h}"));
    }
    if let Some(d) = rec.duration_ms {
        facts.push(format!("**Duration:** {:.1}s", d as f64 / 1000.0));
    }
    if let Some(p) = rec.pages {
        facts.push(format!("**Pages:** {p}"));
    }
    let _ = writeln!(&mut out, "{}", facts.join(" • "));
    let _ = writeln!(&mut out);

    match &rec.description {
        Some(d) => {
            let _ = writeln!(
                &mut out,
                "**Description** (by {}{})",
                rec.described_by.as_deref().unwrap_or("unknown"),
                rec.described_at.map(|t| format!(", unix {t}")).unwrap_or_default()
            );
            let _ = writeln!(&mut out);
            for line in d.lines() {
                let _ = writeln!(&mut out, "> {line}");
            }
            let _ = writeln!(&mut out);
        }
        None => {
            let hint = match rec.kind.as_str() {
                "image" => "view it once, then store what it shows",
                "video" | "audio" => "note what it contains (scenes, speakers, purpose) once",
                "pdf" | "office" => "summarise it once if the extracted text below is not enough",
                _ => "record what this file is for",
            };
            let _ = writeln!(
                &mut out,
                "_No description stored — {hint}: `lens describe {} --text \"…\"`. After that, `lens search` finds it without re-reading the bytes._",
                rec.path
            );
            let _ = writeln!(&mut out);
        }
    }

    // Extracted text excerpt (everything after the summary/description lines).
    let mut lines = rec.body.lines();
    let _ = lines.next();
    let mut rest: Vec<&str> = lines.collect();
    if rest.first().is_some_and(|l| l.starts_with("description: ")) {
        rest.remove(0);
    }
    if !rest.is_empty() {
        let (kept, truncated) = lens_core::fit_lines(&rest, budget, estimate_tokens(&out));
        let _ = writeln!(
            &mut out,
            "**Extracted text** ({} chars via {}{})",
            rec.extracted_chars,
            rec.extractor,
            if truncated { ", excerpt truncated to fit budget" } else { "" }
        );
        let _ = writeln!(&mut out);
        let _ = writeln!(&mut out, "```text");
        for l in kept {
            let _ = writeln!(&mut out, "{l}");
        }
        let _ = writeln!(&mut out, "```");
        let _ = writeln!(&mut out);
    }
    out
}

fn human_size(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, s: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn test_describe_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_with_root(dir.path(), Path::new("a.png"), None, None, false, 500), Err(1));
    }

    #[test]
    fn test_describe_run_show_set_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("assets/logo.png"), &png(64, 64));
        write(&root.join("src/a.rs"), b"pub fn a() {}\n");
        crate::cmd::index::run(Some(root)).unwrap();
        assert_eq!(run_with_root(root, Path::new("assets/logo.png"), None, None, false, 500), Ok(()));
        assert_eq!(run_with_root(root, Path::new("assets/logo.png"), Some("Company logo, blue square"), Some("claude"), false, 500), Ok(()));
        let rec = describe_asset(&lens_core::Storage::open(root.join(".lens/index.db")).unwrap(), "assets/logo.png").unwrap().unwrap();
        assert_eq!(rec.description.as_deref(), Some("Company logo, blue square"));
        assert_eq!(rec.described_by.as_deref(), Some("claude"));
        assert_eq!(run_with_root(root, Path::new("assets/logo.png"), None, None, true, 500), Ok(()));
        assert_eq!(run_with_root(root, Path::new("assets/logo.png"), Some("x"), None, true, 500), Err(2));
        assert_eq!(run_with_root(root, Path::new("src/a.rs"), None, None, false, 500), Err(1), "code files are not assets");
        assert_eq!(run_with_root(root, Path::new("nope.png"), None, None, false, 500), Err(1));
    }

    #[test]
    fn test_render_markdown_with_and_without_description() {
        let rec = AssetRecord {
            path: "doc/spec.pdf".into(),
            kind: "pdf".into(),
            mime: "application/pdf".into(),
            size_bytes: 2048,
            width: None,
            height: None,
            duration_ms: None,
            pages: Some(3),
            extracted_chars: 40,
            extractor: "builtin".into(),
            description: None,
            described_by: None,
            described_at: None,
            body: "[pdf 3 pages application/pdf]\nAcceptance criteria\nMust boot in 2s\n".into(),
            token_estimate: 20,
        };
        let md = render_markdown(&rec, 500);
        assert!(md.contains("# Describe: `doc/spec.pdf`"));
        assert!(md.contains("**Pages:** 3"));
        assert!(md.contains("No description stored"));
        assert!(md.contains("Acceptance criteria"));
        let with = AssetRecord { description: Some("Spec v2".into()), described_by: Some("user".into()), body: "[pdf 3 pages application/pdf]\ndescription: Spec v2\nAcceptance criteria\n".into(), ..rec };
        let md = render_markdown(&with, 500);
        assert!(md.contains("> Spec v2"));
        assert!(md.contains("(by user"));
        assert!(!md.contains("description: Spec v2"), "description line must not leak into the excerpt");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(12), "12 B");
    }
}
