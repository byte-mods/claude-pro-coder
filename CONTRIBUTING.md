# Contributing

Thanks for considering a contribution. This project is small, opinionated, and
intentionally so — keep changes scoped, keep the test suite green, and keep
the safety guards intact.

## Quick reference

- **Issues:** open one before a non-trivial PR — saves rework.
- **PRs:** target `main`. Keep them small and focused.
- **Tests:** `bash scripts/test/round_trip.sh` must report `Total: N, Failures: 0`.
- **License:** MIT (see `LICENSE`). Contributions are accepted under the same.

## Development setup

Clone the repo and install the skill in **symlink mode** so edits to
`pro-coder/SKILL.md` are picked up by Claude Code without re-running install:

```bash
git clone https://github.com/sudeep-dasgupta/claude-skill.git
cd claude-skill
./scripts/install.sh --symlink
```

Symlink mode points `~/.claude/skills/pro-coder` at the repo's
`pro-coder/` directory. Re-running install when the symlink already points
where expected is a no-op.

If you do not have Rust installed, pass `--no-lens` to skip the cargo build
and `--no-mcp` to skip the `~/.claude.json` wire-up. The skill still works
in fallback mode (Read/Grep/Glob).

To switch back to copy mode (or away from the repo entirely):

```bash
./scripts/uninstall.sh
./scripts/install.sh --copy   # or whatever you want
```

## Running the tests

```bash
bash scripts/test/round_trip.sh
```

The suite is self-contained — no external test framework, just bash 3.2+ and
the scripts under test. It covers:

1. `sc_canonicalize_dest` — textual path normalisation in 13 cases.
2. `sc_assert_safe_dest` — the unsafe-dest guard accepts/rejects correctly.
3. Orphan-staging reap on `uninstall.sh`.
4. Install → assert → uninstall → assert in copy mode.
5. Same in symlink mode.
6. Extended-flag forms: `--dest=VALUE`, `--quiet`, `--dry-run` on `install.sh`.

A passing run reports `Total: 61, Failures: 0` (subject to growth as new
tests land — the count is in `CHANGELOG.md` for each release).

The tests use `/tmp` directly, **not** `${TMPDIR}`, because macOS's `${TMPDIR}`
resolves to `/var/folders/...` which the safety guard correctly rejects (it
matches `/var/*`). On macOS `/tmp` is a symlink to `/private/tmp` but
canonicalisation is purely textual, so `/tmp/foo` stays `/tmp/foo` and is
not rejected by the `/private/*` rule.

## Code-style invariants

These are non-negotiable for shell scripts under `scripts/`:

1. **`set -euo pipefail`** at the top of every script.
2. **Default `${HOME:=}`** before any `case` or path-derived expansion —
   `sc_set_default_home` does this. Required because `set -u` would
   otherwise trip on an unset HOME before the unsafe-dest case-match
   gets a chance to refuse the empty default.
3. **Source `_lib.sh`** for the safety helpers. Resolve via `BASH_SOURCE`,
   not `$0`, so resolution survives a symlinked invocation.
4. **Call `sc_assert_safe_dest "${dest}" "<script-name>"`** before any
   destructive action against `${dest}`. The helper canonicalises first
   (closing a `..`-traversal bypass), then case-matches against the
   single source of truth in `_lib.sh`.
5. **Reject `--<flag>` as a value** via `require_value`. Reject empty
   values via `require_eq_value` (the `--flag=` form). Both helpers exit
   2 with a stderr message keyed by the script's name.
6. **All informational output goes through `log()`** which honours
   `--quiet`. Errors go straight to stderr — they remain visible even
   under `--quiet`.
7. **Atomic file operations.** Stage to a sibling temp path, then `mv`
   (POSIX-atomic on the same filesystem). Closes TOCTOU windows during
   partial writes.
8. **`-RP` on `cp`**, not `-R` alone. `-P` preserves symlinks rather than
   following them — guards against attacker-controlled symlink loops.

If a flag adds a value-taking option, support **both** `--flag VALUE` and
`--flag=VALUE` forms. Document this in the script's header comment.

## What goes where

```
.
├── pro-coder/SKILL.md     # the skill itself — Brainiac-OS v5
├── lens/                    # vendored Rust CLI (do not edit in place;
│                            # update via VENDOR.txt SHA bump)
├── scripts/
│   ├── _lib.sh              # shared safety helpers
│   ├── install.sh           # orchestrator
│   ├── install-lens.sh      # cargo build + binary install
│   ├── install-mcp.sh       # safe JSON surgery on ~/.claude.json
│   ├── uninstall.sh         # clean removal
│   └── test/round_trip.sh   # integration test suite
├── README.md                # user-facing entry point
├── CHANGELOG.md             # versioned history
├── CONTRIBUTING.md          # this file
└── LICENSE                  # MIT
```

`lens/` is **vendored** — pinned at the SHA listed in `lens/VENDOR.txt`.
To bump it, replace the `lens/` tree from upstream and update the SHA
in `VENDOR.txt`. Do not edit lens sources in place; cherry-pick the fix
upstream first.

## The 6-phase loop (the way the project gets built)

This repo eats its own dog food. Non-trivial changes go through the same
6-phase loop the `pro-coder` skill enforces — Comprehend, Plan, Implement,
Test, Audit, Section Boundary. Section snapshots live under
`.claude/state/current_section.md`; persistent code-comprehension notes
live under `.claude/state/code-map/`. The `.claude/state/` directory is
**committed** in this repo (per `.claude/state/gitignore_policy`).

If you're not a fan of the protocol, you do not have to follow it — but
you should keep the test suite green and the safety guards intact, and
your PR description should explain the change well enough that a reviewer
can audit it without re-running your investigation.

For the full protocol, see `pro-coder/SKILL.md`.

## Submitting changes

1. **Fork** and branch from `main`.
2. **Make your changes.** Keep the diff small. One concern per PR.
3. **Run the test suite.** `bash scripts/test/round_trip.sh` — must be
   `Failures: 0`. Add new tests for new behaviour.
4. **Update docs.** If you changed CLI surface, update the script's
   header comment, the README's command reference, and `CHANGELOG.md`
   under `[Unreleased]`.
5. **Lint.** `shellcheck scripts/*.sh` should be clean. Markdown should
   render cleanly in GitHub's preview.
6. **Open a PR** against `main`. Reference the issue if there is one.
   Describe the motivation, the change, and the test evidence.

## What we won't merge

- Changes that disable, weaken, or bypass the unsafe-dest guard in
  `_lib.sh` without an extremely good reason.
- New external dependencies in the install scripts. The current
  prerequisites are bash 3.2+, `python3`, `cargo` (optional). Adding
  jq, yq, or a new runtime is a hard sell.
- Edits to `lens/` source. Patch upstream and bump the vendored SHA.
- Wide refactors of `pro-coder/SKILL.md` without prior discussion.
  The skill is feature-complete at v5; behavioural drift here changes
  Claude's behaviour in production for every user.

## Reporting issues

When opening an issue, include:

- The output of `bash scripts/install.sh --help` (or whichever script is
  affected) so we know which version's surface you're using.
- The exact command you ran and what happened.
- Your OS, bash version (`bash --version`), and Rust version
  (`cargo --version`) if the issue touches the lens build.
- The output of `bash scripts/test/round_trip.sh` if relevant.

## Security

If you find a security issue (especially anything that could let a
malicious `--dest` slip past the safety guard, or anything affecting
`~/.claude.json` integrity), please **do not** open a public issue.
Email the maintainer at the address in `git log`'s most recent commit.
