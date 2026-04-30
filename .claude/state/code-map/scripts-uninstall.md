# code-map: uninstall.sh

**Scope:** `scripts/uninstall.sh`
**Last verified:** 2026-04-29 — section 2

## Purpose
Idempotent uninstaller for the super-coder skill, the bundled lens binary, and
the `mcpServers.lens` entry in `claude.json`. Refuses to operate outside the
expected skills directory; reaps orphan staging dirs left by interrupted prior
installs.

## Public API
CLI flags only — no exported symbols.
- `--dest VALUE` (`scripts/uninstall.sh:54`) — skills root, default `~/.claude/skills`.
- `--bin-dir VALUE` (`scripts/uninstall.sh:55`) — lens bin dir, default `~/.claude/bin`.
- `--claude-json VALUE` (`scripts/uninstall.sh:56`) — claude.json path, default `~/.claude.json`.
- `--keep-lens` (`scripts/uninstall.sh:57`) — leave lens binary installed.
- `--keep-mcp` (`scripts/uninstall.sh:58`) — leave `mcpServers.lens` entry.
- `--dry-run` (`scripts/uninstall.sh:59`) — log intended actions, do not act.
- `--quiet` (`scripts/uninstall.sh:60`) — suppress non-error output.

## Invariants
- `set -euo pipefail` (`scripts/uninstall.sh:21`).
- Sources `scripts/_lib.sh` via `BASH_SOURCE`-derived path (`scripts/uninstall.sh:25-27`) so resolution survives symlinked invocation.
- `sc_assert_safe_dest "${dest_root}"` runs BEFORE any destructive action (`scripts/uninstall.sh:71`). Canonicalises the path internally — closes `..`-traversal bypass.
- Defence-in-depth tail check (`scripts/uninstall.sh:79-84`): refuses to operate on a `dest` whose final segment isn't `/super-coder`. Belt-and-braces against the case where `dest_root` passes the safe-dest guard but `${dest_root}/super-coder` somehow resolved to something dangerous (it can't, but the check is cheap).
- Orphan-staging reap (`scripts/uninstall.sh:91-115`) runs BEFORE the early-no-op return so orphans are cleaned up even when no current install exists at `${dest}`.
- `rm -rf "${dest}"` at top level on a symlink unlinks the link, not the target (`scripts/uninstall.sh:124`). Verified by `test_round_trip_symlink_source_skill_md_intact` in `scripts/test/round_trip.sh`.
- Lens-bin cleanup runs `sc_assert_safe_dest "${lens_bin_dir}"` before removing (`scripts/uninstall.sh:138`). `--keep-lens` skips this entirely.

## Concurrency model
N/A — single-threaded interactive script. No locking. Two concurrent uninstalls would race on the `rm -rf` but the worst case is "one wins, the other no-ops".

## Error idioms
- All flag parsing errors go to stderr with prefix `uninstall.sh:` and `exit 2` (`scripts/uninstall.sh:67`).
- Operational errors (e.g. `dest` survives `rm -rf`) → stderr + `exit 1` (`scripts/uninstall.sh:127`).
- Lens-MCP cleanup failures are warnings only (`scripts/uninstall.sh:163`); main uninstall continues to exit 0.

## Callers / callees
- Invoked directly by users; also invoked by `scripts/test/round_trip.sh` as a subprocess.
- Calls `sc_set_default_home` and `sc_assert_safe_dest` (from `_lib.sh`).
- Calls `scripts/install-mcp.sh --remove` (`scripts/uninstall.sh:161`) for the MCP entry cleanup.

## Gotchas
- The `find ... -maxdepth 1 -type d -name '.super-coder.staging.*'` pattern (`scripts/uninstall.sh:114`) intentionally does NOT match the main `super-coder` dir — leading dot + `.staging.` infix make collision impossible.
- `find -type d` does NOT follow symlinks (no `-L`, no `-follow`) — a malicious symlink at `${dest_root}/.super-coder.staging.evil -> /etc` would be skipped. Verified by section super-qa.
- The "Nothing to remove at ${dest}" log message (`scripts/uninstall.sh:118`) fires AFTER the orphan reap. If reaping happened but no main skill was installed, the message reads slightly oddly ("nothing to remove" but per-orphan log lines preceded it). Tracked as cosmetic MINOR; not blocking.
- `--keep-lens` skips both the safe-dest check on `lens_bin_dir` AND the binary removal (`scripts/uninstall.sh:131-133`). This is fine for tests using a non-existent bin dir.

## Open questions
- (none)
