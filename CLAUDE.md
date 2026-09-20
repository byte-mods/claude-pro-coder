# claude-skill project contract

## Project identity

This is the `pro-coder` skill for Claude Code — a two-agent engineering loop
(Brainiac-OS v5) bundled with the `lens` symbol-aware code map. The repo
installs into `~/.claude/skills/pro-coder/`, `~/.claude/bin/lens`, and
`~/.claude.json`.

## Important invariants

### Shell scripts (`scripts/`)

- Every script opens with `set -euo pipefail`, sources `_lib.sh` via
  `BASH_SOURCE`, and calls `sc_set_default_home` before any expansion.
- Call `sc_assert_safe_dest` before any destructive action against a
  user-supplied path. The single source of truth for the unsafe-dest
  case-list lives in `_lib.sh`.
- Atomic file operations: stage to a sibling tmp path in the same
  directory, then `mv` — POSIX-atomic on the same filesystem.
- Copy with `cp -RP` (not `-R` alone) — `-P` preserves symlinks rather
  than following them.
- Value-taking flags support both `--flag VALUE` and `--flag=VALUE`.
  Reject `--flag` followed by another flag, and reject empty `--flag=`.
- All informational output uses `log()` (honours `--quiet`). Errors go
  straight to stderr.
- The `--strict` allow-list and `--allow-root` guard are layered on
  top of the unsafe-dest guard, not replacing it.

### Vendored code

- `lens/` is vendored — pinned at the SHA in `lens/VENDOR.txt`. Do not
  edit lens sources in place; patch upstream and bump the vendored SHA.
- `lens/README.md` is vendored content and may contain stale references
  (e.g. `super-coder`). It is intentionally not edited in-place.

### Skill payload (`pro-coder/`)

- `SKILL.md` is the **core protocol only** and is budgeted at **≤400 lines**
  (`skill_meta.sh` enforces it). Detail belongs in `references/`. The v8
  split exists because a single 844-line file sagged in the middle and the
  lens mandate stopped being followed; letting detail creep back into the
  core restores that failure mode.
- Every `references/*.md` must be pointed at from `SKILL.md`, and every
  pointer in `SKILL.md` must resolve. Both directions are tested.
- `pro-coder/scripts/bootstrap.sh` and `pro-coder/hooks/lens_guard.sh` are
  **standalone** — the skill installs without the repo's `scripts/`, so they
  must not source `_lib.sh`. The `_lib.sh` invariants above govern the
  install pipeline, not the skill payload.
- `lens_guard.sh` **fails open**, always exits 0, and stays inert unless the
  project has both `.lens/index.db` and `.claude/state/pro-coder-guard`. A
  guard that breaks a session is worse than a guard that misses a case.

### JSON surgery

- `scripts/_json_edit.py` and `scripts/_json_edit.js` are behavioural twins.
  Change one, change the other, and cover it in `round_trip.sh` — `_lib.sh`
  picks by runtime availability, so a divergence makes behaviour depend on
  what is on `$PATH`.
- Never gate on `command -v python3`. Windows ships App Execution Alias stubs
  that pass that check and exit 49. Use `sc_json_runtime`, which probes by
  executing.

### Testing

- `bash scripts/test/round_trip.sh` must report `Failures: 0`.
  (Known exception: `round_trip_symlink_install_succeeded` fails under
  MSYS/git-bash on Windows without Developer Mode, where `ln -s` makes a
  directory copy rather than a link.)
- `bash scripts/test/skill_meta.sh` must report `Failures: 0`.
- The suite passes from any directory; uses `/tmp` directly (not
  `${TMPDIR}`, which macOS resolves under `/var/` — tripping the guard).
- Add new tests for new behaviour; update the assertion count in docs.

### Release process

- The project version lives in `VERSION` at the repo root.
- `CHANGELOG.md` follows Keep a Changelog. Pre-1.0, minor bumps may
  break compatibility.
- `scripts/_lib.sh` exposes `sc_version` which reads `VERSION`.
- All four scripts (`install.sh`, `uninstall.sh`, `install-lens.sh`,
  `install-mcp.sh`) support `--version`.

### Dependencies

- Runtime prerequisites: bash 3.2+, git, python3, cargo (optional).
  No new external dependencies for the install pipeline without a
  compelling reason (jq, yq, etc. are hard sells).

### Security-sensitive code

- `sc_assert_safe_dest` in `_lib.sh` — the unsafe-dest case-list. Must
  never be weakened. Covers macOS and Linux system paths.
- `install-mcp.sh` — JSON surgery on `~/.claude.json`. Always backs up
  first; atomic write via Python `os.replace`.
- `sc_canonicalize_dest` — closes `..`-traversal bypasses. Must run
  before the case-match in `sc_assert_safe_dest`.

## File map

| Path | Purpose |
|---|---|
| `pro-coder/SKILL.md` | Skill definition (Brainiac-OS v8) — core protocol only |
| `pro-coder/references/` | Phase mechanics, output format, modes, checklists, memory — loaded on demand |
| `pro-coder/scripts/bootstrap.sh` | One-call P1 bootstrap (standalone; does not source `_lib.sh`) |
| `pro-coder/hooks/lens_guard.sh` | `PreToolUse` guard enforcing lens-before-Grep/Read |
| `scripts/install-hooks.sh` | Registers/removes the guard in `~/.claude/settings.json` |
| `scripts/_json_edit.py` / `.js` | Shared JSON editor (behavioural twins) |
| `scripts/test/skill_meta.sh` | Meta-tests for the skill bundle |
| `lens/` | Vendored Rust CLI (do not edit in place) |
| `scripts/_lib.sh` | Shared safety helpers |
| `scripts/install.sh` | Orchestrator |
| `scripts/install-lens.sh` | Cargo build + binary install |
| `scripts/install-mcp.sh` | Safe JSON surgery on `~/.claude.json` |
| `scripts/uninstall.sh` | Clean removal |
| `scripts/test/round_trip.sh` | Integration test suite |
| `VERSION` | Single source of truth for the project version |
| `CHANGELOG.md` | Versioned history |
| `CONTRIBUTING.md` | Dev workflow, code-style invariants |
| `README.md` | User-facing entry point |
