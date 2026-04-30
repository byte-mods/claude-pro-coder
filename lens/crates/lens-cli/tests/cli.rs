use std::process::Command;

const LENS_BIN: &str = env!("CARGO_BIN_EXE_lens");

#[test]
fn test_lens_cli_version_flag_prints_version() {
    let out = Command::new(LENS_BIN)
        .arg("--version")
        .output()
        .expect("failed to spawn lens binary");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("lens "), "expected 'lens <ver>', got: {stdout}");
    assert!(
        stdout.split_whitespace().nth(1).is_some_and(|s| s.contains('.')),
        "expected dotted version, got: {stdout}"
    );
}

#[test]
fn test_lens_cli_short_version_flag_prints_version() {
    let out = Command::new(LENS_BIN)
        .arg("-V")
        .output()
        .expect("failed to spawn lens binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("lens "));
}

#[test]
fn test_lens_cli_no_args_prints_help() {
    let out = Command::new(LENS_BIN)
        .output()
        .expect("failed to spawn lens binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("symbol-aware"), "help missing tagline: {stdout}");
    assert!(stdout.contains("USAGE"), "help missing USAGE section: {stdout}");
}

#[test]
fn test_lens_cli_help_flag_prints_clap_help() {
    let out = Command::new(LENS_BIN)
        .arg("--help")
        .output()
        .expect("failed to spawn lens binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"), "clap help missing 'Usage:' line: {stdout}");
    assert!(stdout.contains("Commands:"), "clap help missing 'Commands:' section: {stdout}");
}

#[test]
fn test_lens_cli_short_help_flag_prints_clap_help() {
    let out = Command::new(LENS_BIN)
        .arg("-h")
        .output()
        .expect("failed to spawn lens binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Usage:"));
}

#[test]
fn test_lens_cli_unknown_token_exits_two_with_explanatory_error() {
    let out = Command::new(LENS_BIN)
        .arg("xyzzy_definitely_not_a_path_nor_subcommand")
        .output()
        .expect("failed to spawn lens binary");
    assert_eq!(out.status.code(), Some(2), "expected exit 2, got {:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a known subcommand") || stderr.contains("does not exist"),
        "expected explanatory error, got: {stderr}"
    );
}

#[test]
fn test_lens_cli_unknown_flag_exits_two_with_clap_error() {
    let out = Command::new(LENS_BIN)
        .arg("--definitely-not-a-real-flag")
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error") || stderr.contains("unexpected"));
}

#[test]
fn test_lens_cli_query_subcommand_errors_when_index_missing() {
    // Updated in Section 4: `lens query` is no longer a stub. Without a
    // built `.lens/index.db` the command exits 1 and points the user at
    // `lens index`.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(LENS_BIN)
        .current_dir(dir.path())
        .args(["query", "what is OrderService"])
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("lens query"),
        "expected stderr to mention `lens query`, got: {stderr}"
    );
    assert!(
        stderr.contains("lens index") || stderr.contains("does not exist"),
        "expected stderr to point at `lens index`, got: {stderr}"
    );
}

#[test]
fn test_lens_cli_follow_subcommand_routes_to_real_follow() {
    // Real `follow` (S5/T2 conversion). The contract pin: when no
    // `.lens/index.db` exists in cwd, the binary errors with exit 1
    // and a stderr message pointing at `lens index`.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(LENS_BIN)
        .args(["follow", "Foo", "--from", "src/x.rs:42", "--budget", "500"])
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("lens follow"),
        "expected stderr to mention `lens follow`, got: {stderr}"
    );
    assert!(
        stderr.contains("lens index") || stderr.contains("does not exist"),
        "expected stderr to point at `lens index`, got: {stderr}"
    );
}

#[test]
fn test_lens_cli_graphify_compat_dot_update_routes_to_update() {
    // Updated in Section 4: `lens . --update` now invokes real `lens update`,
    // not a stub. Use an isolated tempdir without `.lens/index.db` so the
    // real command errors with exit 1 ("run lens index first") — that's the
    // documented contract for `update` against an un-indexed project.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
    let out = Command::new(LENS_BIN)
        .args([dir.path().to_str().unwrap(), "--update"])
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("lens update"),
        "expected stderr to mention `lens update`, got: {stderr}"
    );
    assert!(
        stderr.contains("lens index") || stderr.contains("does not exist"),
        "expected stderr to point user at `lens index`, got: {stderr}"
    );
}

#[test]
fn test_lens_cli_graphify_compat_path_only_routes_to_index() {
    // Updated in Section 3: `lens <path>` now invokes real index, not a stub.
    // Use an isolated tempdir to avoid indexing the lens repo itself when this
    // test runs from cargo's workspace root.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").unwrap();
    let out = Command::new(LENS_BIN)
        .arg(dir.path())
        .output()
        .expect("failed to spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lens index:"), "stdout missing index summary: {stdout}");
    assert!(dir.path().join(".lens").join("index.db").exists());
}

#[test]
fn test_lens_cli_meter_subcommand_with_json_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(LENS_BIN)
        .args(["meter", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"input_tokens\":"), "expected JSON object: {stdout}");
    assert!(stdout.contains("\"output_tokens\":"));
    assert!(stdout.contains("\"calls\":"));
}

#[test]
fn test_lens_cli_mcp_subcommand_runs_initialize_handshake() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    // Spawn `lens mcp`, write an `initialize` request, read back the
    // response, then close stdin to let the loop exit cleanly.
    let mut child = Command::new(LENS_BIN)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lens mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{}}}}").unwrap();
    }
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"protocolVersion\":\"2024-11-05\""), "stdout: {stdout}");
    assert!(stdout.contains("\"name\":\"lens\""));
}
