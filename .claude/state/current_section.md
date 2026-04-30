# Section Snapshot — 2026-04-29 (claude-skill section 3)

## Just completed
- Project: claude-skill (`/Users/sudeepdasgupta/projects/claude-skill`).
- Section: 3 — comprehensive README rewrite.
- Tasks closed: T1.
- Closure summary:
  - **T1** (`README.md`, full rewrite, ~620 lines): restructured the user-facing entry point. Added prominent **Features** table, **Command reference** covering all four scripts (`install.sh`, `install-lens.sh`, `install-mcp.sh`, `uninstall.sh`) plus `scripts/test/round_trip.sh` plus the full `lens` CLI subcommand surface. Added explicit **How Claude uses it** walkthrough — bootstrap, 6-phase loop with tool-per-phase mapping, MCP tool surface (`lens_follow` / `lens_refs` / `lens_query` / `lens_explain` / `lens_path` / `lens_slice` / `lens_map`), five persistence layers. Updated **What got installed where** to include `~/.claude.json` (MCP entry) and the `~/.claude.json.bak.*` backup pattern. Documented `--no-mcp` and `--claude-json` flags missing from prior README. Added a dedicated **Tests** section. Per-task super-qa: PASS (zero defects after one MINOR coverage gap closed inline — `lens <PATH>` graphify-compat positional form added to lens index-lifecycle list).
- Section-level super-qa: skipped intentionally. Single-task section touching one file; per-task super-qa already cross-checked the README against `cli.rs`, all four scripts, `SKILL.md`, and the test suite. Section-level pass would have re-read the same diff with the same context — no compositional surface to verify.
- Test totals: re-ran `bash scripts/test/round_trip.sh` during super-qa cross-check → `Total: 49, Failures: 0`. No new tests this section (docs-only).

## Code-map updates this section
- No new or revised code-map notes. README is documentation, not a code area with invariants / public API / callers. New install-pipeline understanding is already captured in the existing scripts' headers and in the prior section's code-map notes (`scripts-safety-helpers.md`, `scripts-uninstall.md`, `scripts-test-suite.md`).

## Verified facts carried forward
- Lens CLI surface (verified at `lens/crates/lens-cli/src/cli.rs:25-136`): 13 subcommands — `init`, `index`, `update`, `query`, `follow`, `refs`, `slice`, `add`, `path`, `explain`, `map`, `meter`, `watch`, `mcp`. Plus top-level positional `path: Option<PathBuf>` (`cli.rs:17-18`) + `--update` flag (`cli.rs:21-22`) for graphify-compat.
- Lens default budgets: `query --budget 2000` (`cli.rs:47`), `follow --budget 2000` (`cli.rs:58`), `slice --budget 2000` (`cli.rs:74`). README's example `--budget 1500` invocations are explicit overrides, not defaults.
- Lens version pinned at `0.1.0` (`lens/Cargo.toml:9`).
- Lens vendored SHA pinned at `a29f523` (`lens/VENDOR.txt`).
- MCP tool names (registered surface, per `scripts/install-mcp.sh:2-4`): `lens_follow`, `lens_refs`, `lens_query`, `lens_explain`, `lens_path`, `lens_slice`, `lens_map`. The actual tool implementation lives inside the `lens mcp` server (in the binary); `install-mcp.sh` only registers `command: <lens-bin> args: ["mcp"]` in `~/.claude.json`.
- Install orchestration (`scripts/install.sh:171-200`): `install.sh` calls `install-lens.sh` (skipped if `--no-lens`) then `install-mcp.sh` (skipped if `--no-mcp` OR `--no-lens`). Both sub-scripts are independently runnable.
- `install-mcp.sh` uses Python's `os.replace` for atomic JSON write (`install-mcp.sh:186`). Backup at `${claude_json}.bak.YYYYMMDD-HHMMSS` before any write (`install-mcp.sh:174-177`).
- Test suite output line is exactly `Total: 49, Failures: 0` — verified by running.

## Open invariants for next section
All Section 1 + Section 2 invariants still hold:
- Any new shell script under `scripts/` MUST use `set -euo pipefail`, default `${HOME:=}`, source `_lib.sh`, reject `--<flag>` as a value, surface errors to stderr.
- Tests must use `/tmp/...` explicitly, NOT `${TMPDIR}`. The safety guard correctly rejects `/var/*`; macOS `${TMPDIR}` lives there.
- Cleanup-tracking arrays inside helpers invoked via `$(fn)` are silently broken — use a single parent dir with one `rm -rf` instead.
- The `sc_canonicalize_dest` helper is purely textual. If a future change needs symlink resolution, add a SEPARATE helper.
- The `.claude/state/` directory is `commit` per `.claude/state/gitignore_policy`.

New invariant from this section:
- Any change to lens CLI surface (add/remove/rename a subcommand or flag) MUST update three places: `lens-cli/src/cli.rs` (source of truth), `README.md`'s "lens CLI — full subcommand surface" section, AND the MCP tool list in `scripts/install-mcp.sh:2-4` if it's a tool exposed via MCP. The README's claim of which tools Claude can call is grounded in `install-mcp.sh`'s comment header — that's where the MCP-exposed surface is canonically declared from claude-skill's perspective.

## Next section
- **Goal:** pick from the deferred backlog below — or close the project as feature-complete.
- **Entry blast radius:** depends on chosen task.
- **Open questions for next session:**
  - Is shellcheck-via-CI worth adding now that the test suite gives CI a real signal to gate on? Cheap (~10 lines GitHub Actions).
  - Should the README's MCP tool list cite the lens binary's source file rather than `install-mcp.sh`'s comment? Currently the comment is the user-facing source of truth for which MCP tools are claimed; the actual implementation isn't read in this repo.

## Remaining work (handed back to user)

### Tier B — discoverability / polish (carried over from Section 1)
1. `install.sh --dry-run` and `install.sh --quiet` for symmetry with `uninstall.sh`.
2. `install.sh --dest=VALUE` form (currently only `--dest VALUE` works).
3. CHANGELOG.md — versioned history.
4. CONTRIBUTING.md — fork/edit/upstream flow.
5. CI — GitHub Actions running `bash scripts/test/round_trip.sh` + `shellcheck scripts/*.sh` + Markdown-lint.

### Tier C — nice-to-have, low priority (carried over)
6. `examples/` directory — sample prompts.
7. Meta-tests for `super-coder/SKILL.md` — frontmatter parser, internal reference checker.
8. EUID/root warning in scripts when run as root.
9. `--strict` flag with positive-allow-list path validation.

### Section 2 deferred MINORs (low priority)
10. `uninstall.sh:118` — "Nothing to remove at ${dest}" message reads oddly when orphans were just reaped.

### Section 3 deferred MINORs (super-qa noted, all stylistic)
11. README `lens init` "appends to .gitignore unless suppressed" phrasing — could be reworded to match `cli.rs:33`'s exact help text "Skip modifying .gitignore". Cosmetic.
12. README `lens --version` claim of `0.1.0` is accurate (verified at `Cargo.toml:9`) but not surfaced as a verifiable assertion in the docs. Cosmetic.

### Tier D — explicitly out of scope
13. `super-coder/SKILL.md` — feature-complete at v5.

## CLAUDE.md proposals queued
- 0 new proposals appended this section. Reason: this section was a docs rewrite; no project-wide invariants were discovered that aren't already in the existing scripts' comments, the prior code-map notes, or the README itself. The 4 prior proposals targeting the lens repo's future CLAUDE.md remain in `.claude/state/claude_md_proposals.md`.

## Resume protocol reminder
- For claude-skill resume: read `README.md` (now ~620 lines, comprehensive), `super-coder/SKILL.md`, the four `scripts/*.sh` files plus `scripts/_lib.sh` and `scripts/test/round_trip.sh`, this snapshot, and the three notes under `.claude/state/code-map/`. Re-verify every claim against current source before acting.
- For lens resume (separate repo at `~/projects/lens/`): the queued CLAUDE.md proposals in `.claude/state/claude_md_proposals.md` still target lens, not claude-skill.
