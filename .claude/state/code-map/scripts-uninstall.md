# code-map: uninstall.sh

**Scope:** `scripts/uninstall.sh`
**Last verified:** 2026-04-30 — section 4

## Purpose
Idempotent uninstaller for the pro-coder skill, the bundled lens binary, and
the `mcpServers.lens` entry in `claude.json`. Refuses to operate outside the
expected skills directory; reaps orphan staging dirs left by interrupted prior
installs.

## Public API
CLI flags only — no exported symbols.
- `--dest VALUE` / `--dest=VALUE` (`scripts/uninstall.sh:70-71`) — skills root, default `~/.claude/skills`.
- `--bin-dir VALUE` / `--bin-dir=VALUE` (`scripts/uninstall.sh:72-73`) — lens bin dir, default `~/.claude/bin`.
- `--claude-json VALUE` / `--claude-json=VALUE` (`scripts/uninstall.sh:74-75`) — claude.json path, default `~/.claude.json`.
- `--keep-lens` (`scripts/uninstall.sh:76`) — leave lens binary installed.
- `--keep-mcp` (`scripts/uninstall.sh:77`) — leave `mcpServers.lens` entry.
- `--dry-run` (`scripts/uninstall.sh:78`) — log intended actions, do not act.
- `--quiet` (`scripts/uninstall.sh:79`) — suppress non-error output.
- `--strict` (`scripts/uninstall.sh:80`) — positive allow-list: refuse paths outside `~/.claude/`.
- `--allow-root` (`scripts/uninstall.sh:81`) — opt in to running as root (default: refuse).

## Invariants
- `set -euo pipefail` (`scripts/uninstall.sh:25`).
- Sources `scripts/_lib.sh` via `BASH_SOURCE`-derived path (`scripts/uninstall.sh:30`) so resolution survives symlinked invocation.
- Default `${HOME}` via `sc_set_default_home` (`scripts/uninstall.sh:33`) before any path computation.
- Refuses `EUID=0` unless `--allow-root` (`scripts/uninstall.sh:89-93` block routing through `sc_assert_not_root`).
- `sc_assert_safe_dest "${dest_root}"` runs BEFORE any destructive action (`scripts/uninstall.sh:95`). Canonicalises the path internally — closes `..`-traversal bypass.
- Defence-in-depth tail check (`scripts/uninstall.sh:112-115`): refuses to operate on a `dest` whose final segment isn't `/pro-coder`. Belt-and-braces against the case where `dest_root` passes the safe-dest guard but `${dest_root}/pro-coder` somehow resolved to something dangerous (it can't, but the check is cheap).
- Orphan-staging reap (`scripts/uninstall.sh:127-142`) runs BEFORE the early-no-op return so orphans are cleaned up even when no current install exists at `${dest}`. Tracks an `orphans_reaped` counter for the reap-aware log message.
- `rm -rf "${dest}"` at top level on a symlink unlinks the link, not the target (`scripts/uninstall.sh:172+`). Verified by `test_round_trip_symlink_source_skill_md_intact` in `scripts/test/round_trip.sh`.
- Lens-bin cleanup runs `sc_assert_safe_dest "${lens_bin_dir}"` before removing (`scripts/uninstall.sh:188`). `--keep-lens` short-circuits both the safe-dest check and the binary removal at `:185-186`.

## Concurrency model
N/A — single-threaded interactive script. No locking. Two concurrent uninstalls would race on the `rm -rf` but the worst case is "one wins, the other no-ops".

## Error idioms
- All flag parsing errors go to stderr with prefix `uninstall.sh:` and `exit 2` (`scripts/uninstall.sh:83-87` block).
- Operational errors (e.g. `dest` does not end in `/pro-coder`, or `dest` survives `rm -rf`) → stderr + `exit 1` (`scripts/uninstall.sh:114-115`).
- Lens-MCP cleanup failures are warnings only (`scripts/uninstall.sh:199`); main uninstall continues to exit 0.

## Callers / callees
- Invoked directly by users; also invoked by `scripts/test/round_trip.sh` as a subprocess.
- Calls `sc_set_default_home`, `sc_assert_safe_dest`, `sc_assert_not_root`, `sc_assert_strict_allowed`, `require_value`, `require_eq_value` (from `_lib.sh`).
- Calls `scripts/install-mcp.sh --remove` (`scripts/uninstall.sh:199` area) for the MCP entry cleanup.

## Gotchas
- The `find ... -maxdepth 1 -type d -name '.pro-coder.staging.*'` pattern (`scripts/uninstall.sh:142`) intentionally does NOT match the main `pro-coder` dir — leading dot + `.staging.` infix make collision impossible.
- `find -type d` does NOT follow symlinks (no `-L`, no `-follow`) — a malicious symlink at `${dest_root}/.pro-coder.staging.evil -> /etc` would be skipped. Verified by section super-qa.
- Reap-aware log differentiation (`scripts/uninstall.sh:147-153`): when nothing exists at `${dest}` AND `orphans_reaped > 0`, the "reaped N orphan staging dir(s)" message fires instead of "Already uninstalled". Resolves a prior cosmetic glitch where the latter fired even after orphan reaping had logged its own per-orphan lines.
- `--keep-lens` skips both the safe-dest check on `lens_bin_dir` AND the binary removal (`scripts/uninstall.sh:185-186`). This is fine for tests using a non-existent bin dir.

## Open questions
- (none)
