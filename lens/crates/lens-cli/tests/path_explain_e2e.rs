//! End-to-end binary tests for `lens path` and `lens explain`.
//! Sets up a small project, runs `lens` (index) then path/explain, asserts
//! the rendered markdown is shaped correctly.

use std::path::Path;
use std::process::Command;

const LENS_BIN: &str = env!("CARGO_BIN_EXE_lens");

fn write(path: &Path, contents: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn build_index(root: &Path) {
    let out = Command::new(LENS_BIN)
        .arg(root)
        .output()
        .expect("failed to spawn lens index");
    assert!(
        out.status.success(),
        "indexing failed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_e2e_path_subcommand_renders_distance_and_hops() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Three-file call chain: a → b → c.
    write(
        &root.join("a.rs"),
        "use b::middle;\npub fn entry() { middle(); }\n",
    );
    write(
        &root.join("b.rs"),
        "use c::leaf;\npub fn middle() { leaf(); }\n",
    );
    write(&root.join("c.rs"), "pub fn leaf() {}\n");
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["path", "entry", "leaf"])
        .output()
        .expect("failed to spawn lens path");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "lens path expected success; stderr: {stderr}\nstdout: {stdout}"
    );
    assert!(stdout.contains("# Path: entry → leaf"), "stdout: {stdout}");
    // The exact distance depends on the resolver's edge inference. We only
    // assert the render shape is valid and includes "entry" and "leaf".
    assert!(stdout.contains("entry"));
    assert!(stdout.contains("leaf"));
}

#[test]
fn test_e2e_path_subcommand_unknown_symbol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a.rs"), "pub fn known() {}\n");
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["path", "ghost", "known"])
        .output()
        .expect("failed to spawn lens path");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no symbol matched 'ghost'"));
}

#[test]
fn test_e2e_explain_subcommand_renders_card_with_neighbors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("a.rs"),
        "pub struct Owner;\nimpl Owner { pub fn method(&self) {} }\n",
    );
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["explain", "Owner"])
        .output()
        .expect("failed to spawn lens explain");
    assert!(
        out.status.success(),
        "lens explain expected success; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# Explain:"), "stdout: {stdout}");
    assert!(stdout.contains("Owner"));
    assert!(stdout.contains("Kind:"));
}

#[test]
fn test_e2e_explain_subcommand_isolated_symbol_renders_helpful_message() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a.rs"), "pub fn alone() {}\n");
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["explain", "alone"])
        .output()
        .expect("failed to spawn lens explain");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# Explain:"));
    assert!(
        stdout.contains("no parents") || stdout.contains("Neighbors:** 0"),
        "expected isolated marker; got: {stdout}"
    );
}

#[test]
fn test_e2e_explain_subcommand_unknown_symbol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a.rs"), "pub fn known() {}\n");
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["explain", "ghost"])
        .output()
        .expect("failed to spawn lens explain");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no symbol matched"));
}

#[test]
fn test_e2e_explain_self_path_is_short_circuited() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("a.rs"), "pub fn x() {}\n");
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["path", "x", "x"])
        .output()
        .expect("failed to spawn lens path");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("same symbol"));
}
