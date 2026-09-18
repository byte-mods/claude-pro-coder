//! `lens map` — architecture summary of the indexed project (or a sub-tree).
//!
//! Wraps [`lens_core::build_map`] and [`lens_core::render_map`] for the CLI;
//! responsible only for path resolution, error surfacing, and stdout I/O.

use std::path::Path;

use lens_core::{build_map, estimate_tokens, render_map, Storage};

pub fn run(scope: Option<&Path>, depth: u32, budget: u32) -> Result<(), u8> {
    let cwd = match crate::cmd::util::cwd_project_root("map") {
        Ok(p) => p,
        Err(code) => return Err(code),
    };
    run_with_root(&cwd, scope, depth, budget)
}

pub fn run_with_root(root: &Path, scope: Option<&Path>, depth: u32, budget: u32) -> Result<(), u8> {
    let (storage, _db_path) = match crate::cmd::util::open_with_auto_freshness(root, "map") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };

    // Normalise scope to a project-relative string with `/` separators. We
    // accept either an absolute path (rebased to project root) or a relative
    // path; in both cases the storage layer expects forward slashes.
    let scope_str: Option<String> = match scope {
        Some(p) => {
            let s = if p.is_absolute() {
                match p.strip_prefix(root) {
                    Ok(rel) => rel.to_string_lossy().to_string(),
                    Err(_) => {
                        eprintln!(
                            "lens map: --scope '{}' is not under project root '{}'",
                            p.display(),
                            root.display()
                        );
                        return Err(1);
                    }
                }
            } else {
                p.to_string_lossy().to_string()
            };
            Some(s.replace('\\', "/"))
        }
        None => None,
    };

    let rendered = match render_within_budget(&storage, scope_str.as_deref(), depth, budget) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lens map: build failed: {e}");
            return Err(1);
        }
    };
    crate::cmd::util::finish(root, &storage, rendered, &[]);
    Ok(())
}

/// Render the map at the requested depth; if the rendering exceeds
/// `budget` tokens, reduce the depth one level at a time until it fits (or
/// depth 0 is reached). The note appended tells the model how much was
/// folded so it can re-query with `--scope` instead of a bigger budget.
pub fn render_within_budget(
    storage: &Storage,
    scope: Option<&str>,
    depth: u32,
    budget: u32,
) -> lens_core::Result<String> {
    let mut d = depth;
    loop {
        let result = build_map(storage, scope, d)?;
        let mut rendered = render_map(&result);
        let est = estimate_tokens(&rendered);
        if est <= budget || d == 0 {
            if d < depth {
                rendered.push_str(&format!(
                    "\n_depth reduced {depth} → {d} to fit --budget {budget} (est {est} tokens); narrow with --scope for detail_\n"
                ));
            }
            return Ok(rendered);
        }
        d -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, s: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, s).unwrap();
    }

    fn build_initial_index(root: &Path) {
        crate::cmd::index::run(Some(root)).expect("initial index");
    }

    #[test]
    fn test_map_run_errors_when_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = run_with_root(dir.path(), None, 2, 2000);
        assert_eq!(r, Err(1));
    }

    #[test]
    fn test_map_run_succeeds_on_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Init an empty index.
        crate::cmd::init::run(Some(root), false).unwrap();
        crate::cmd::index::run(Some(root)).unwrap();
        let r = run_with_root(root, None, 2, 2000);
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn test_map_run_succeeds_on_indexed_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("src/a.rs"), "pub fn hello() {}\n");
        write(&root.join("src/b.rs"), "pub fn world() {}\n");
        write(&root.join("tests/x.rs"), "pub fn test() {}\n");
        build_initial_index(root);
        assert_eq!(run_with_root(root, None, 2, 2000), Ok(()));
        assert_eq!(run_with_root(root, Some(Path::new("src")), 2, 2000), Ok(()));
    }

    #[test]
    fn test_map_budget_reduces_depth_until_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..12 {
            write(&root.join(format!("src/mod{i}/deep/file.rs")), &format!("pub fn f{i}() {{}}\n"));
        }
        build_initial_index(root);
        let storage = Storage::open(root.join(".lens/index.db")).unwrap();
        let full = render_within_budget(&storage, None, 3, 100_000).unwrap();
        let tight = render_within_budget(&storage, None, 3, 60).unwrap();
        assert!(estimate_tokens(&full) > 60, "fixture must exceed the tight budget");
        assert!(tight.contains("depth reduced 3 →"), "{tight}");
        assert!(tight.len() < full.len());
    }

    #[test]
    fn test_map_run_rejects_absolute_scope_outside_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("a.rs"), "pub fn hi() {}\n");
        build_initial_index(root);
        // Absolute path outside root → error.
        let outside = std::path::PathBuf::from("/some/other/place");
        let r = run_with_root(root, Some(&outside), 2, 2000);
        assert_eq!(r, Err(1));
    }
}
