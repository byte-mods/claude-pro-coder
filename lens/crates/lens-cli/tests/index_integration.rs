use std::fs;
use std::path::Path;
use std::process::Command;

const LENS_BIN: &str = env!("CARGO_BIN_EXE_lens");

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn test_index_subcommand_in_cwd_writes_to_dot_lens() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("util.rs"), "pub fn helper() -> u32 { 1 }\n");

    let out = Command::new(LENS_BIN)
        .arg("index")
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn lens index");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = dir.path().join(".lens").join("index.db");
    assert!(db.exists(), "expected .lens/index.db at {}", db.display());
}

#[test]
fn test_index_positional_path_indexes_target_dir() {
    // `lens <path>` (graphify-compat) — equivalent to `cd <path> && lens index`.
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a.rs"), "pub fn one() -> u32 { 1 }\n");

    let out = Command::new(LENS_BIN)
        .arg(dir.path())
        .output()
        .expect("failed to spawn lens <path>");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join(".lens").join("index.db").exists());
}

#[test]
fn test_index_subcommand_summary_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a.rs"), "pub fn alpha() {}\n");
    write(&dir.path().join("b.rs"), "pub fn beta() {}\n");

    let out = Command::new(LENS_BIN)
        .arg("index")
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wrote 2 files"),
        "stdout missing file count: {stdout}"
    );
    assert!(
        stdout.contains("symbols"),
        "stdout missing symbol count: {stdout}"
    );
    assert!(
        stdout.contains("resolved"),
        "stdout missing resolve summary: {stdout}"
    );
}

#[test]
fn test_index_positional_nonexistent_path_errors() {
    let out = Command::new(LENS_BIN)
        .arg("/this/path/definitely/does/not/exist")
        .output()
        .expect("failed to spawn");
    // Bare-positional onto a path that doesn't exist returns the
    // "not a known subcommand and does not exist as a path" message
    // (handled by main.rs:32-39, exit 2).
    assert!(!out.status.success());
    let code = out.status.code().unwrap_or(-1);
    assert!(code == 2 || code == 1, "expected exit 1 or 2, got {code}");
}

#[test]
fn test_index_cross_file_resolution_end_to_end() {
    // Verify the full pipeline → insert → resolve chain produces a usable
    // index with the cross-file import resolved. Project layout is chosen
    // so the Rust extractor's qname (`util::helper`) matches the import's
    // raw_path (`util::helper`).
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("util.rs"), "pub fn helper() -> u32 { 1 }\n");
    write(
        &dir.path().join("main.rs"),
        "use util::helper;\nfn main() { let _ = helper(); }\n",
    );

    let out = Command::new(LENS_BIN)
        .arg("index")
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The summary line should report at least one resolved import.
    assert!(
        stdout.contains("resolved 0 refs / 0 calls / 0 types / 1 imports")
            || stdout.contains("/ 1 imports across files"),
        "stdout did not show the expected resolved-imports summary: {stdout}"
    );
}
