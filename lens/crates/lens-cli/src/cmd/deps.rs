//! `lens deps <file>` — file-level connections: imports in both directions,
//! call edges aggregated per counterpart file, and the file's most-called
//! symbols. The cheapest way to decide whether a file is in a change's
//! blast radius without reading it.

use std::path::Path;

use lens_core::{file_deps, FileDeps};

pub fn run(path: &Path) -> Result<(), u8> {
    let cwd = match crate::cmd::util::cwd_project_root("deps") {
        Ok(p) => p,
        Err(code) => return Err(code),
    };
    run_with_root(&cwd, path)
}

pub fn run_with_root(root: &Path, path: &Path) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "deps") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let Some(rel) = lens_core::docs::scope_to_relative(root, path) else {
        eprintln!("lens deps: '{}' is not under project root '{}'", path.display(), root.display());
        return Err(1);
    };
    let result = match file_deps(&storage, &rel) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "lens deps: '{rel}' is not in the symbol index (unsupported language, gitignored, or not yet indexed). \
                 Try `lens search` for text-only files."
            );
            return Err(1);
        }
        Err(e) => {
            eprintln!("lens deps: lookup failed: {e}");
            return Err(1);
        }
    };
    let rendered = render_markdown(&result);
    crate::cmd::util::finish(root, &storage, rendered, &[rel.as_str()]);
    Ok(())
}

/// Pure markdown formatter — no fs / no Storage.
pub fn render_markdown(d: &FileDeps) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# Deps: `{}`", d.path);
    let _ = writeln!(&mut out);
    let _ = writeln!(
        &mut out,
        "**Language:** {} • **Symbols:** {} • **Imports:** {} out / {} in • **Calls:** {} file{} out / {} file{} in",
        d.language,
        d.symbol_count,
        d.imports_out.len(),
        d.importers_in.len(),
        d.calls_out.len(),
        if d.calls_out.len() == 1 { "" } else { "s" },
        d.calls_in.len(),
        if d.calls_in.len() == 1 { "" } else { "s" }
    );
    let _ = writeln!(&mut out);

    if !d.imports_out.is_empty() {
        let _ = writeln!(&mut out, "## Imports ({})", d.imports_out.len());
        for e in &d.imports_out {
            match &e.resolved_path {
                Some(p) => {
                    let _ = writeln!(&mut out, "- `{}` → `{p}`", e.raw_path);
                }
                None => {
                    let _ = writeln!(&mut out, "- `{}` _(external)_", e.raw_path);
                }
            }
        }
        let _ = writeln!(&mut out);
    }
    if !d.importers_in.is_empty() {
        let _ = writeln!(&mut out, "## Imported by ({})", d.importers_in.len());
        for e in &d.importers_in {
            let _ = writeln!(
                &mut out,
                "- `{}` (as `{}`)",
                e.resolved_path.as_deref().unwrap_or("?"),
                e.raw_path
            );
        }
        let _ = writeln!(&mut out);
    }
    if !d.calls_out.is_empty() {
        let _ = writeln!(&mut out, "## Calls into ({})", d.calls_out.len());
        for e in &d.calls_out {
            let _ = writeln!(&mut out, "- `{}` — {} call site{}", e.path, e.call_sites, if e.call_sites == 1 { "" } else { "s" });
        }
        let _ = writeln!(&mut out);
    }
    if !d.calls_in.is_empty() {
        let _ = writeln!(&mut out, "## Called from ({})", d.calls_in.len());
        for e in &d.calls_in {
            let _ = writeln!(&mut out, "- `{}` — {} call site{}", e.path, e.call_sites, if e.call_sites == 1 { "" } else { "s" });
        }
        let _ = writeln!(&mut out);
    }
    if !d.top_symbols.is_empty() {
        let _ = writeln!(&mut out, "## Top symbols ({})", d.top_symbols.len());
        for s in &d.top_symbols {
            let _ = writeln!(
                &mut out,
                "- `{}` ({}) — line {} [{} caller{}]",
                s.qualified_name,
                s.kind,
                s.start_line,
                s.caller_count,
                if s.caller_count == 1 { "" } else { "s" }
            );
        }
        let _ = writeln!(&mut out);
    }
    if d.imports_out.is_empty() && d.importers_in.is_empty() && d.calls_out.is_empty() && d.calls_in.is_empty() {
        let _ = writeln!(&mut out, "_No import or call edges recorded for this file._");
        let _ = writeln!(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lens_core::{CallEdge, ImportEdge, MapSymbol};
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    #[test]
    fn test_deps_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(run_with_root(dir.path(), Path::new("a.rs")), Err(1));
    }

    #[test]
    fn test_deps_run_end_to_end_on_python_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("pkg/__init__.py"), "");
        write(&root.join("pkg/util.py"), "def helper():\n    return 1\n");
        write(&root.join("pkg/app.py"), "from pkg.util import helper\n\ndef main():\n    return helper()\n");
        crate::cmd::index::run(Some(root)).unwrap();
        assert_eq!(run_with_root(root, Path::new("pkg/util.py")), Ok(()));
        assert_eq!(run_with_root(root, &root.join("pkg/app.py")), Ok(()), "absolute path accepted");
        assert_eq!(run_with_root(root, Path::new("pkg/missing.py")), Err(1));
    }

    #[test]
    fn test_render_markdown_sections() {
        let d = FileDeps {
            path: "pkg/util.py".into(),
            language: "python".into(),
            symbol_count: 2,
            imports_out: vec![ImportEdge { raw_path: "os".into(), resolved_path: None }],
            importers_in: vec![ImportEdge { raw_path: "pkg.util.helper".into(), resolved_path: Some("pkg/app.py".into()) }],
            calls_out: vec![],
            calls_in: vec![CallEdge { path: "pkg/app.py".into(), call_sites: 2 }],
            top_symbols: vec![MapSymbol { qualified_name: "pkg.util.helper".into(), kind: "function".into(), file_path: "pkg/util.py".into(), start_line: 1, caller_count: 2 }],
        };
        let md = render_markdown(&d);
        assert!(md.contains("# Deps: `pkg/util.py`"));
        assert!(md.contains("`os` _(external)_"));
        assert!(md.contains("## Imported by (1)"));
        assert!(md.contains("`pkg/app.py` (as `pkg.util.helper`)"));
        assert!(md.contains("## Called from (1)"));
        assert!(md.contains("2 call sites"));
        assert!(md.contains("[2 callers]"));
    }
}
