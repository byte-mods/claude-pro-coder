use std::fs;
use std::path::{Path, PathBuf};

use lens_core::storage::Storage;

const DEFAULT_CONFIG: &str = "\
# Lens project config — edit to suit your repo.

[index]
languages = [\"rust\", \"python\", \"typescript\", \"javascript\", \"go\", \"dart\", \"java\"]

[slice]
default_budget = 2000
";

const GITIGNORE_ENTRY: &str = ".lens/";

pub fn run(path: Option<&Path>, no_gitignore: bool) -> Result<(), u8> {
    let project_root: PathBuf = match path {
        Some(p) => p.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(e) => {
                eprintln!("lens init: cannot resolve current directory: {e}");
                return Err(1);
            }
        },
    };
    if !project_root.exists() {
        eprintln!("lens init: '{}' does not exist", project_root.display());
        return Err(1);
    }
    if !project_root.is_dir() {
        eprintln!("lens init: '{}' is not a directory", project_root.display());
        return Err(1);
    }

    let lens_dir = project_root.join(".lens");
    if let Err(e) = fs::create_dir_all(&lens_dir) {
        eprintln!("lens init: cannot create '{}': {e}", lens_dir.display());
        return Err(1);
    }

    let db_path = lens_dir.join("index.db");
    let was_new_db = !db_path.exists();
    let storage = match Storage::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lens init: failed to open index database: {e}");
            return Err(1);
        }
    };
    let schema_version = storage.version().unwrap_or(0);

    let config_path = lens_dir.join("config.toml");
    let mut wrote_config = false;
    if !config_path.exists() {
        if let Err(e) = fs::write(&config_path, DEFAULT_CONFIG) {
            eprintln!("lens init: cannot write '{}': {e}", config_path.display());
            return Err(1);
        }
        wrote_config = true;
    }

    let mut gitignore_action: &str = "skipped (--no-gitignore)";
    if !no_gitignore {
        match ensure_gitignore_entry(&project_root) {
            Ok(GitignoreUpdate::Created) => gitignore_action = "created",
            Ok(GitignoreUpdate::Appended) => gitignore_action = "appended",
            Ok(GitignoreUpdate::AlreadyPresent) => gitignore_action = "already present",
            Err(e) => {
                eprintln!(
                    "lens init: gitignore update failed (non-fatal): {e}"
                );
                gitignore_action = "failed";
            }
        }
    }

    let verb = if was_new_db { "initialized" } else { "refreshed" };
    println!(
        "lens init: {verb} {} (schema v{schema_version}, config {}, gitignore {})",
        lens_dir.display(),
        if wrote_config { "written" } else { "kept" },
        gitignore_action,
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum GitignoreUpdate {
    Created,
    Appended,
    AlreadyPresent,
}

fn ensure_gitignore_entry(project_root: &Path) -> std::io::Result<GitignoreUpdate> {
    let path = project_root.join(".gitignore");
    if !path.exists() {
        let body = format!("{GITIGNORE_ENTRY}\n");
        fs::write(&path, body)?;
        return Ok(GitignoreUpdate::Created);
    }
    let existing = fs::read_to_string(&path)?;
    if existing.lines().any(|l| {
        let trimmed = l.trim();
        trimmed == GITIGNORE_ENTRY
            || trimmed == ".lens"
            || trimmed == "/.lens"
            || trimmed == "/.lens/"
    }) {
        return Ok(GitignoreUpdate::AlreadyPresent);
    }
    let mut new = existing;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(GITIGNORE_ENTRY);
    new.push('\n');
    fs::write(&path, new)?;
    Ok(GitignoreUpdate::Appended)
}
