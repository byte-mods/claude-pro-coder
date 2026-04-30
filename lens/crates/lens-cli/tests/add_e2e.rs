//! End-to-end binary tests for `lens add`. Uses local `file://` URLs so
//! the test does not depend on network availability.

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
    assert!(out.status.success());
}

#[test]
fn test_e2e_add_python_file_via_file_url_lands_in_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_index(root);

    let payload_dir = tempfile::tempdir().unwrap();
    let src = payload_dir.path().join("module.py");
    write(&src, "def fetched_func():\n    return 42\n");

    let url = format!("file://{}", src.display());
    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["add", &url])
        .output()
        .expect("failed to spawn lens add");
    assert!(
        out.status.success(),
        "expected lens add to succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("saved"), "stdout: {stdout}");
    assert!(stdout.contains("indexed"), "stdout: {stdout}");

    // Verify via lens query that the new symbol is there.
    let q = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["query", "fetched_func"])
        .output()
        .unwrap();
    assert!(q.status.success());
    let qstdout = String::from_utf8_lossy(&q.stdout);
    assert!(
        qstdout.contains("fetched_func"),
        "expected query to find the added symbol; got: {qstdout}"
    );
}

#[test]
fn test_e2e_add_unknown_extension_saves_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_index(root);

    let payload_dir = tempfile::tempdir().unwrap();
    let src = payload_dir.path().join("notes.txt");
    write(&src, "this is a doc, not source code\n");

    let url = format!("file://{}", src.display());
    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["add", &url])
        .output()
        .expect("failed to spawn lens add");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("saved-only"));
}

#[test]
fn test_e2e_add_dedups_on_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_index(root);

    let payload_dir = tempfile::tempdir().unwrap();
    let src = payload_dir.path().join("dup.py");
    write(&src, "def x(): pass\n");

    let url = format!("file://{}", src.display());
    let r1 = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["add", &url])
        .output()
        .unwrap();
    assert!(r1.status.success());
    let r2 = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["add", &url])
        .output()
        .unwrap();
    assert!(r2.status.success());
    let stdout2 = String::from_utf8_lossy(&r2.stdout);
    assert!(stdout2.contains("dedup"), "expected dedup marker; got: {stdout2}");
}

#[test]
fn test_e2e_add_invalid_url_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_index(root);

    let out = Command::new(LENS_BIN)
        .current_dir(root)
        .args(["add", "this is not a url"])
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(1));
}
