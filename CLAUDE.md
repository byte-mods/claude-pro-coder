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

### Testing

- `bash scripts/test/round_trip.sh` must report `Failures: 0`.
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
| `pro-coder/SKILL.md` | Skill definition (Brainiac-OS v5) |
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
