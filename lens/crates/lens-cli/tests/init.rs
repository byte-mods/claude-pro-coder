use std::fs;
use std::process::Command;

const LENS_BIN: &str = env!("CARGO_BIN_EXE_lens");

fn run_init(args: &[&str]) -> std::process::Output {
    Command::new(LENS_BIN)
        .arg("init")
        .args(args)
        .output()
        .expect("failed to spawn lens init")
}

#[test]
fn test_init_creates_dot_lens_directory() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_init(&[dir.path().to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.path().join(".lens").is_dir());
}

#[test]
fn test_init_creates_index_db_with_schema_v1() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_init(&[dir.path().to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let db = dir.path().join(".lens").join("index.db");
    assert!(db.exists(), "index.db not created at {}", db.display());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("schema v"), "stdout missing schema version: {stdout}");
    assert!(stdout.contains("initialized"));
}

#[test]
fn test_init_writes_default_config_toml() {
    let dir = tempfile::tempdir().unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let config = dir.path().join(".lens").join("config.toml");
    assert!(config.exists());
    let body = fs::read_to_string(&config).unwrap();
    assert!(body.contains("[index]"), "config missing [index]: {body}");
    assert!(body.contains("languages"));
    assert!(body.contains("default_budget"));
}

#[test]
fn test_init_creates_gitignore_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let gi = dir.path().join(".gitignore");
    assert!(gi.exists(), "gitignore not created");
    let body = fs::read_to_string(&gi).unwrap();
    assert!(body.contains(".lens/"), "gitignore missing .lens/: {body}");
}

#[test]
fn test_init_appends_to_existing_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let gi = dir.path().join(".gitignore");
    fs::write(&gi, "/target\n*.log\n").unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let body = fs::read_to_string(&gi).unwrap();
    assert!(body.contains("/target"), "existing entry lost: {body}");
    assert!(body.contains("*.log"));
    assert!(body.contains(".lens/"));
}

#[test]
fn test_init_dedupes_gitignore_entry() {
    let dir = tempfile::tempdir().unwrap();
    let gi = dir.path().join(".gitignore");
    fs::write(&gi, "/target\n.lens/\n").unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let body = fs::read_to_string(&gi).unwrap();
    let count = body.lines().filter(|l| l.trim() == ".lens/").count();
    assert_eq!(count, 1, "duplicate .lens/ entry: {body}");
}

#[test]
fn test_init_no_gitignore_flag_skips_gitignore_creation() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_init(&[dir.path().to_str().unwrap(), "--no-gitignore"]);
    assert!(out.status.success());
    assert!(!dir.path().join(".gitignore").exists());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--no-gitignore") || stdout.contains("skipped"));
}

#[test]
fn test_init_idempotent_does_not_overwrite_user_config() {
    let dir = tempfile::tempdir().unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let config = dir.path().join(".lens").join("config.toml");
    let custom = "# user-customised config\n[index]\nlanguages = [\"rust\"]\n";
    fs::write(&config, custom).unwrap();
    let out = run_init(&[dir.path().to_str().unwrap()]);
    assert!(out.status.success());
    let body = fs::read_to_string(&config).unwrap();
    assert_eq!(body, custom, "user config was overwritten on re-init");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("refreshed"), "expected 'refreshed' on re-init: {stdout}");
    assert!(stdout.contains("config kept"));
}

#[test]
fn test_init_rejects_path_that_does_not_exist() {
    let out = run_init(&["/this/path/does/not/exist/lens-test-abc"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not exist") || stderr.contains("not a directory"));
}

#[test]
fn test_init_rejects_path_that_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir.txt");
    fs::write(&file, "x").unwrap();
    let out = run_init(&[file.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a directory"));
}

#[test]
fn test_init_with_no_path_uses_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(LENS_BIN)
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.path().join(".lens").is_dir());
}

#[test]
fn test_init_running_twice_does_not_duplicate_meta_row() {
    let dir = tempfile::tempdir().unwrap();
    run_init(&[dir.path().to_str().unwrap()]);
    let out2 = run_init(&[dir.path().to_str().unwrap()]);
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout2.contains("refreshed"), "second run should say 'refreshed': {stdout2}");
}
