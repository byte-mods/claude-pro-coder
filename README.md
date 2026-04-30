# pro-coder

> A Claude Code skill that turns Claude into a two-agent engineering team — a hyper-rigorous **architect** that plans and implements, and an adversarial **QA reviewer** that gates every task. They iterate until the work is provably correct, not just plausibly correct.

Bundled with [`lens`](#lens--token-efficient-retrieval) — a symbol-aware code map that lets Claude pull minimal slices of your codebase instead of dumping whole files into context. Token-efficient, structural retrieval. Wired up as an MCP server so Claude calls lens verbs as structured tools, not Bash.

Invoke with `/pro-coder` inside Claude Code.

---

## Quickstart

For a typical macOS / Linux dev machine with Rust + Claude Code already installed:

```bash
git clone https://github.com/sudeep-dasgupta/claude-skill.git ~/code/claude-skill
cd ~/code/claude-skill
./scripts/install.sh
```

The script installs three things:

1. The skill at `~/.claude/skills/pro-coder/SKILL.md` — Claude Code auto-discovers it.
2. The bundled `lens` binary at `~/.claude/bin/lens` (built from `lens/` via `cargo build --release`, ~30s first run, cached after).
3. An `mcpServers.lens` entry in `~/.claude.json` — Claude Code spawns lens at startup as a structured-tool server.

Open Claude Code in any project and type `/pro-coder` followed by your engineering objective.

If you don't have Rust, the install still works — the skill auto-detects the missing binary and falls back to `Read`/`Grep`/`Glob`. See [Prerequisites](#prerequisites).

---

## Table of contents

- [Features](#features)
- [Prerequisites](#prerequisites)
- [Install](#install)
- [Verify](#verify)
- [What got installed where](#what-got-installed-where)
- [Use it inside Claude Code](#use-it-inside-claude-code)
- [How Claude uses it](#how-claude-uses-it)
- [Command reference](#command-reference)
- [Lens — token-efficient retrieval](#lens--token-efficient-retrieval)
- [How it works — two agents, one team](#how-it-works--two-agents-one-team)
- [The 6-phase loop (Brainiac-OS v5)](#the-6-phase-loop-brainiac-os-v5)
- [Examples](#examples)
- [Update](#update)
- [Uninstall](#uninstall)
- [Tests](#tests)
- [Troubleshooting](#troubleshooting)
- [Customisation](#customisation)
- [FAQ](#faq)
- [License](#license)

---

## Features

| | What | Where it lives |
|---|---|---|
| **Two-agent QA loop** | An adversarial **super-qa** subagent gates every task and every section. Read-only, structured verdicts, unbounded loop with stuck-loop detection. PASS requires zero `BLOCKER` and zero `MAJOR` defects. | `pro-coder/SKILL.md` (P4.5, P5) |
| **6-phase workflow** | Comprehend → Research → Plan → Implement+Test → Audit → Section Boundary. Active phase declared on every response. | `pro-coder/SKILL.md` |
| **Symbol-aware retrieval (lens)** | Budget-capped `lens follow`, `lens query`, `lens refs`, `lens slice`, `lens explain`, `lens path`, `lens map`. ~1500 tokens for a function definition + signature + body + callers regardless of file size. | `lens/` crate, `~/.claude/bin/lens` |
| **MCP-native tool surface** | Lens runs as an MCP stdio server (`lens mcp`) auto-wired into `~/.claude.json`. Claude calls `lens_follow`, `lens_refs`, `lens_query`, `lens_explain`, `lens_path`, `lens_slice`, `lens_map` as structured tools — no Bash boilerplate, no string-parsing of stdout. | `scripts/install-mcp.sh`, `~/.claude.json` |
| **Persistent code-map** | Per-area Markdown notes under `<project>/.claude/state/code-map/` capturing API shapes, invariants, callers, gotchas with `file:line` anchors. Survives across sessions; reconciled at every section close. | `<project>/.claude/state/code-map/*.md` |
| **Section snapshots** | At each section boundary (5+ tasks or explicit `section boundary` keyword), the agent writes a structured snapshot of verified facts, open invariants, and the next-section blast radius — then stops. The next session resumes from the snapshot, not from a stale conversation tail. | `<project>/.claude/state/current_section.md` |
| **CLAUDE.md proposal queue** | The agent never writes to `CLAUDE.md`. Suggested project-contract additions are appended (with file:line justification + confidence) to `<project>/.claude/state/claude_md_proposals.md` for your review. | `<project>/.claude/state/claude_md_proposals.md` |
| **Auto-fallback** | If lens isn't on `$PATH` or the project has no supported language files (lens supports Rust, Python, TypeScript, JavaScript, Go), the skill detects this once at bootstrap and swaps `lens query`/`lens follow` for `Read`/`Grep`/`Glob`. The loop still runs end-to-end. | bootstrap step 5 in `SKILL.md` |
| **Token meter** | `lens meter` keeps a persistent input/output token tally across `/clear`s and sessions. `--diff` since last call, `--since 1h`, `--json` for scripts. | lens binary |
| **Idempotent install + atomic file ops** | `install.sh` re-runs are no-ops when source matches dest (SKILL.md byte-equality + lens source-hash). Copy mode stages into a sibling tmp dir then `mv` (same-FS atomic). Symlink mode replaces real dirs explicitly. Uninstall reaps orphan staging dirs from interrupted prior installs. | `scripts/install.sh`, `scripts/uninstall.sh`, `scripts/_lib.sh` |
| **Safe-dest guard** | All destructive operations refuse to run on `/`, `$HOME`, or any system path (`/etc`, `/var`, `/usr`, `/private`, `/Applications`, `/Network`, `/Volumes`, `/System`, `/Library`, `/opt`, `/boot`, `/dev`, `/proc`, `/sys`, `/bin`, `/sbin`, `/home`, `/root`, `/srv`, `/run`, `/lib`, `/lib64`, `/mnt`, `/media`). Paths are canonicalised first — `..`-traversal bypasses (e.g. `--dest ~/skills/../../../etc`) trip the guard. | `scripts/_lib.sh` |
| **Committed regression suite** | `scripts/test/round_trip.sh` runs 73 assertions: canonicalisation, safe-dest guard, orphan-staging reap, copy + symlink round-trips, idempotency, extended-flag forms, SKILL.md meta-tests (frontmatter, required sections, P-ref resolution, code-fence balance), and the `--strict` allow-list + root-refusal guards. Self-cleaning, CI-friendly. | `scripts/test/round_trip.sh` |

---

## Prerequisites

| Required | What | How to get it |
|---|---|---|
| ✅ Always | **Claude Code CLI** (`claude` command) | [Anthropic install docs](https://docs.anthropic.com/en/docs/claude-code/quickstart) |
| ✅ Always | **bash 3.2+** and **git** | Default on macOS / most Linux |
| ⚠️ Recommended | **Rust toolchain** (`cargo` on `$PATH`) — needed to build the bundled `lens` binary | [rustup.rs](https://rustup.rs) one-liner: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| ⚠️ Recommended | **Python 3** (`python3` on `$PATH`) — used by `install-mcp.sh` for safe JSON surgery on `~/.claude.json` | Default on macOS / most Linux |

**If `cargo` is missing:** `install-lens.sh` prints a warning and exits 0 (skill installs without lens; `install-mcp.sh` is then skipped because there's no binary to register). The skill detects the missing binary at first run in each project and falls back to `Read` / `Grep` / `Glob`. The 6-phase loop still runs end-to-end; you just lose lens's token-efficient symbol slicing. You can install Rust later and re-run `./scripts/install.sh` to get lens + MCP.

**If `python3` is missing:** `install-mcp.sh` prints the JSON snippet you need to add to `~/.claude.json` manually and exits 1. Skill + lens still install; only the MCP wire-up is skipped. Lens still works via the Bash CLI in that case — you just lose the structured-tool path.

**Lens language scope (today):** Rust, Python, TypeScript (`.ts`/`.tsx`), JavaScript (`.js`/`.jsx`/`.mjs`/`.cjs`), Go (`.go`). On unsupported codebases, `lens index` produces an empty index and the skill auto-falls back per project. More languages are upstream work in [`lens`](https://github.com/sudeep-dasgupta/lens).

---

## Install

### Step 1 — Clone the repo

```bash
git clone https://github.com/sudeep-dasgupta/claude-skill.git ~/code/claude-skill
```

(Replace `sudeep-dasgupta` if you forked it.)

### Step 2 — Run the install script

```bash
cd ~/code/claude-skill
./scripts/install.sh
```

`install.sh` is the orchestrator. It runs three steps:

1. **Skill copy** — copies `pro-coder/SKILL.md` → `~/.claude/skills/pro-coder/SKILL.md`. Atomic rename via a staging dir; refuses to write under unsafe paths via the safe-dest guard.
2. **Lens build** — invokes `scripts/install-lens.sh` to `cargo build --release` the `lens/` crate and install the binary at `~/.claude/bin/lens`. Skips on missing cargo with a warning. Idempotent: re-runs are no-ops when the lens source hash matches the marker at `~/.claude/bin/.lens.installed.sha`.
3. **MCP wire-up** — invokes `scripts/install-mcp.sh` to add the `mcpServers.lens` entry to `~/.claude.json`. Backs up first (`~/.claude.json.bak.YYYYMMDD-HHMMSS`); atomic JSON write via Python; touches only the lens key, every other key in `claude.json` is preserved byte-for-byte.

The whole sequence is **idempotent**. Re-running with no source changes is a no-op for all three steps.

**install.sh flags:**

| Flag | Default | Effect |
|---|---|---|
| `--symlink` | off | Symlink the skill instead of copying. `git pull` upgrades the skill in place — best for skill developers. |
| `--copy` | on | Explicit copy mode (the default). Mutually exclusive with `--symlink`. |
| `--force` | off | Overwrite existing destination + force lens rebuild. |
| `--no-lens` | off | Skip the lens build entirely (also implies `--no-mcp`). Skill runs in fallback mode. |
| `--no-mcp` | off | Build lens but skip the `~/.claude.json` wire-up. Lens still works via Bash. |
| `--dest DIR` | `~/.claude/skills` | Custom skills root. |
| `--bin-dir DIR` | `~/.claude/bin` | Custom lens binary destination. |
| `--claude-json PATH` | `~/.claude.json` | Custom claude.json path. |

### Step 3 — Add `~/.claude/bin` to `$PATH` *(only if needed)*

If the install script printed:

```
install-lens.sh: NOTE — /Users/you/.claude/bin is not on your PATH.
```

then add this to your `~/.zshrc` or `~/.bashrc`:

```bash
export PATH="$HOME/.claude/bin:$PATH"
```

Reload your shell (`source ~/.zshrc` or open a new terminal).

This is **optional** — when lens runs as an MCP server (the default), Claude Code launches the binary by absolute path from `~/.claude.json`, so `$PATH` doesn't matter for the skill. PATH only matters if you want to run `lens` directly from your terminal.

### Manual install (no script)

If you'd rather do it yourself:

```bash
mkdir -p ~/.claude/skills ~/.claude/bin

# Skill
cp -r ~/code/claude-skill/pro-coder ~/.claude/skills/

# Lens binary
(cd ~/code/claude-skill/lens && cargo build --release)
cp ~/code/claude-skill/lens/target/release/lens ~/.claude/bin/lens
chmod +x ~/.claude/bin/lens

# MCP wire-up — add this to ~/.claude.json under "mcpServers":
#   "lens": { "command": "/Users/you/.claude/bin/lens", "args": ["mcp"] }
```

---

## Verify

```bash
# 1. Skill is installed
ls ~/.claude/skills/pro-coder/
# expected output: SKILL.md

# 2. Lens binary works (only if you installed lens)
lens --version
# expected output: lens 0.1.0 (matches lens/Cargo.toml workspace version)
# if "command not found": ~/.claude/bin isn't on PATH (see Step 3 above) — that's fine,
# Claude Code still calls lens by absolute path via the MCP entry in ~/.claude.json

# 3. Smoke-test lens on a supported-language project (Rust, Python, TS/JS, Go)
cd ~/some-project
lens init
lens index
# If the project uses a supported language: "lens index: wrote N files / N symbols / ... to .lens/index.db"
# If the project is unsupported (Ruby, Java, C++, etc.): "lens index: wrote 0 files, 0 symbols" — this
# is expected; the skill auto-detects an empty index and falls back to Read/Grep/Glob at runtime.
# See the FAQ for the full language list.

# 4. Inspect the MCP wire-up
grep -A2 '"lens"' ~/.claude.json
# expected:
#   "lens": {
#     "command": "/Users/you/.claude/bin/lens",
#     "args": ["mcp"]
#   },

# 5. Run the install/uninstall round-trip test suite
bash scripts/test/round_trip.sh
# expected output ends with: "Total: 73, Failures: 0"
# tests canonicalisation, safe-dest guard, orphan-staging reap, and
# both copy + symlink install/uninstall round-trips against /tmp jails.
```

---

## What got installed where

**User-global (across all projects):**

| Path | Purpose |
|---|---|
| `~/.claude/skills/pro-coder/SKILL.md` | The skill definition. Loaded by Claude Code at startup. |
| `~/.claude/bin/lens` | The bundled lens binary (if cargo was available). |
| `~/.claude/bin/.lens.installed.sha` | Content hash of vendored lens source — used for idempotent rebuild detection. |
| `~/.claude.json` | Claude Code config; gets the `mcpServers.lens` entry pointing at the binary. Backed up to `~/.claude.json.bak.YYYYMMDD-HHMMSS` before any write. |
| `~/.claude/agent-memory/brainiac-os/` | Cross-session durable memory (user prefs, validated feedback, stakeholder context). |

**Per-project, on first `/pro-coder` invocation:**

| Path | Purpose | Lifetime | Owner |
|---|---|---|---|
| `<project>/.claude/state/gitignore_policy` | One-line marker (`ignore` or `commit`) so the bootstrap question is asked once per project | Project | agent |
| `<project>/.claude/state/code-map/` | Persistent symbol-area notes with `file:line` anchors | Project | agent (writes), user (reads) |
| `<project>/.claude/state/current_section.md` | Snapshot at section boundary — verified facts, open invariants, next-section blast radius | Overwritten each P6 | agent |
| `<project>/.claude/state/claude_md_proposals.md` | Append-only queue of suggested `CLAUDE.md` additions for **your** review | Project | agent (write) / user (review) |
| `<project>/.lens/index.db` | SQLite symbol index used by `lens query`/`lens follow` | Project | agent (refreshed at P5) |
| `<project>/.lens/freshness.txt` | Auto-freshness throttle marker (5s default window) | Project | lens |
| `<project>/CLAUDE.md` | Project contract / conventions | Project lifetime | **user only** — agent has read-only access |

**Source repo (from your clone):**

| Path | Purpose |
|---|---|
| `~/code/claude-skill/pro-coder/SKILL.md` | The skill source. Edit + reinstall to customise. |
| `~/code/claude-skill/lens/` | Vendored lens crate (Rust workspace: `lens-core` + `lens-cli`). Pinned at `lens/VENDOR.txt`. |
| `~/code/claude-skill/scripts/` | `install.sh`, `install-lens.sh`, `install-mcp.sh`, `uninstall.sh`, `_lib.sh`, `test/round_trip.sh`. |

---

## Use it inside Claude Code

Claude Code auto-discovers skills at startup by scanning `~/.claude/skills/`. There's no extra config — once `pro-coder/SKILL.md` lives there, `/pro-coder` becomes a slash command available in every project.

### Open Claude Code in your project

```bash
cd ~/your-project
claude
```

### Trigger the skill

Type a slash command followed by your engineering goal:

```
/pro-coder build a sharded connection pool with backpressure for our Tokio service
```

```
/pro-coder the ingest pipeline drops messages under load — find the cause and fix it
```

```
/pro-coder refactor src/auth/session.rs to remove the Mutex on the hot path
```

### Ending a section

When a coherent unit of work finishes, type:

```
section boundary
```

The skill writes its snapshot, announces, and stops. Then run `/clear` (or `/compact` if you want to keep some history) and start the next section fresh. The next session reads `.claude/state/current_section.md` plus the relevant code-map notes — verified facts and open invariants carry forward; stale conversation context does not.

---

## How Claude uses it

When you invoke `/pro-coder <goal>`, the skill loads its system prompt and Claude takes the architect role. Here is what happens, mechanically, in order.

### 1. Bootstrap *(once per project)*

Before the first task, Claude:

- Creates `.claude/state/` and `.claude/state/code-map/` if missing.
- Asks **once** whether `.claude/state/` should be gitignored or committed (`a` or `b`); answer is recorded in `.claude/state/gitignore_policy` and never asked again.
- Reads `CLAUDE.md` if present; surfaces a one-line note if absent (does not create it).
- Detects the lens binary on `$PATH` and the `.lens/index.db` state. Runs `lens init && lens index` on first encounter. If lens is missing OR the project has 0 supported-language symbols, the project mode is **fallback** for the rest of the session — `lens` calls become `Read`/`Grep`/`Glob`.

### 2. The 6-phase loop

```
P1 Comprehend ─► P2 Research ─► P3 Plan ─► P4 Implement+Test ─► P4.5 Super-QA ─► P5 Audit ─► P6 Boundary
                                                                       │
                                                                       └─► loop until VERDICT: PASS
```

Active phase is declared at the top of every response (e.g. `## P4 — T2: orphan-staging reap`).

| Phase | What Claude does | Tools |
|---|---|---|
| **P1 Comprehend** | Restate goal. Read `CLAUDE.md` + prior section snapshot. Build the blast-radius code-map for the task: load existing notes from `.claude/state/code-map/` and verify them against current source. | `lens query`, `lens follow`, `Read` |
| **P2 Research** | Read every file in the blast radius end-to-end. Cite `file:line`. Catalog idioms, invariants, failure modes. | `lens follow`, `lens refs`, `Read`, `Grep` |
| **P3 Plan** | Decompose into atomic tasks (≤100 LOC each, named verifying tests). Wait for ack only on big plans (>5 files, new dep, public API change, CI change). | (text) |
| **P4 Implement & Test** | One task at a time. Idiomatic code, mental compile, tests in the same task, run the full suite. | `Edit`, `Write`, `Bash` |
| **P4.5 Super-QA** | Spawn an isolated `Agent(subagent_type: general-purpose)` with the task spec, file list, and test names. Subagent has zero memory of the conversation; rebuilds its own understanding from current source. Returns `VERDICT: PASS` or `FAIL` with `BLOCKER`/`MAJOR`/`MINOR` defects. Loop until PASS. | `Agent` |
| **P5 Audit** | Requirement-traceability matrix. Adversarial review (empty/max/malformed input, partial failures, memory pressure). **Section-level super-qa pass over the cumulative diff.** Update code-map notes. Run `lens . --update`. | `Agent`, `Edit`, `Bash` |
| **P6 Boundary** | Write `.claude/state/current_section.md`. Append CLAUDE.md proposals if any. Announce, **stop.** User runs `/clear`. | `Write` |

### 3. The MCP tool surface

When MCP wire-up landed (`install-mcp.sh` ran successfully), Claude Code spawns lens at startup and exposes its verbs as **structured tools** rather than via Bash invocations. Claude calls them like any other tool — JSON Schema input, response as content blocks, no string-parsing of stdout.

The tools currently registered:

| Tool name | Wraps | Purpose |
|---|---|---|
| `lens_follow` | `lens follow <symbol> --budget N` | "Ctrl+Click" — doc + signature + body + callers in ~budget tokens |
| `lens_refs` | `lens refs <symbol> --limit N` | List call/reference sites with `file:line` |
| `lens_query` | `lens query "<topic>" --budget N` | Symbol-graph BFS/DFS seeds for a topic |
| `lens_explain` | `lens explain <symbol>` | Plain-language summary of a symbol + neighbours |
| `lens_path` | `lens path "A" "B"` | Shortest connection between two symbols |
| `lens_slice` | `lens slice <file>:<line> --budget N` | Minimal context around a location |
| `lens_map` | `lens map [--scope DIR] [--depth N]` | Architecture summary by directory |

Without MCP, Claude falls back to `Bash` invocations of the same lens commands. The skill works either way; MCP is the lower-friction path.

### 4. Where state lives between sessions

Five layers, each with its own lifetime:

| Layer | Lives in | Cleared by |
|---|---|---|
| **Conversation context** | the running session | `/clear`, P6 boundary |
| **Section snapshot** | `<project>/.claude/state/current_section.md` | next P6 overwrites it |
| **Code-map** | `<project>/.claude/state/code-map/*.md` | manual user edit only |
| **CLAUDE.md** *(project contract)* | `<project>/CLAUDE.md` | user edit only — **agent never writes here** |
| **Agent memory** *(durable, cross-session)* | `~/.claude/agent-memory/brainiac-os/` | explicit user request |

Code-map and agent memory are the two long-lived layers. **Code-map** holds anything derivable from current source (API shapes, invariants, callers, gotchas — every fact has a `file:line` anchor). **Agent memory** holds anything *not* derivable from source (why a constraint exists, who asked for it, team review preferences). If you can answer the question by reading the code, it goes in code-map. If you can only answer it by knowing the human context, it goes in agent memory.

---

## Command reference

### Skill commands *(inside Claude Code)*

| Command | Purpose |
|---|---|
| `/pro-coder <goal>` | Invoke the skill. Goal is one or two lines; constraints/perf budgets/deadlines optional but useful. |
| `section boundary` | Force a P6 reset at any point. The skill snapshots state, appends any CLAUDE.md proposals, and stops. |
| `reset` | Synonym for `section boundary`. |

### `scripts/install.sh` — orchestrator

Installs the skill, builds + installs lens, registers the MCP server. Idempotent.

```
./scripts/install.sh                           # default: copy + lens + MCP
./scripts/install.sh --symlink                 # symlink the skill (developer mode)
./scripts/install.sh --force                   # overwrite + force lens rebuild
./scripts/install.sh --no-lens                 # skip lens build (implies --no-mcp)
./scripts/install.sh --no-mcp                  # build lens but skip claude.json edit
./scripts/install.sh --dest DIR                # custom skills root
./scripts/install.sh --bin-dir DIR             # custom lens binary dest
./scripts/install.sh --claude-json PATH        # custom claude.json
./scripts/install.sh --dry-run                 # print intent; make no changes
./scripts/install.sh --quiet                   # suppress non-error output
./scripts/install.sh --strict                  # extra paranoia — refuse paths outside ~/.claude/
./scripts/install.sh --allow-root              # opt in to running as root (default: refuse)
./scripts/install.sh -h | --help               # full flag list
```

Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE` forms (e.g. `--dest=/custom`). Empty values via `--flag=` are explicitly rejected.

`--dry-run` is read-only end-to-end: it skips the cargo build entirely (logging `would build lens via install-lens.sh`), passes `--dry-run` through to `install-mcp.sh`, and never writes to the skills directory or `claude.json`.

`--strict` is layered on top of the unsafe-dest guard, not replacing it. With `--strict`, every operative path (`--dest`, `--bin-dir`, `--claude-json`) must resolve to a location under `~/.claude/`. Anything outside is refused. Useful when paths come from a config file the user does not fully control.

`--allow-root` is required when running as root (e.g. inside a CI container). Without it, every script refuses to run with `EUID=0` and exits 1 — running as root creates root-owned files under `$HOME` that the user's normal shell cannot edit later.

### `scripts/install-lens.sh` — build + install lens binary

Called by `install.sh` automatically. Can be re-run standalone (e.g. after a Rust toolchain upgrade).

```
./scripts/install-lens.sh                      # build + install
./scripts/install-lens.sh --bin-dir DIR        # custom dest (default ~/.claude/bin)
./scripts/install-lens.sh --force              # force rebuild even if hash matches
./scripts/install-lens.sh --quiet              # suppress non-error output
./scripts/install-lens.sh --skip-if-no-cargo   # default — exit 0 with warning if cargo missing
./scripts/install-lens.sh --require-cargo      # exit non-zero if cargo missing
```

### `scripts/install-mcp.sh` — register lens as MCP server

Called by `install.sh` automatically. Can be re-run standalone or with `--remove` to uninstall just the MCP entry.

```
./scripts/install-mcp.sh                       # add/update mcpServers.lens
./scripts/install-mcp.sh --lens-bin PATH       # custom lens binary path
./scripts/install-mcp.sh --claude-json PATH    # custom claude.json
./scripts/install-mcp.sh --remove              # remove mcpServers.lens
./scripts/install-mcp.sh --dry-run             # print diff, do not write
./scripts/install-mcp.sh --quiet               # suppress non-error output
```

Always backs up `claude.json` to `claude.json.bak.YYYYMMDD-HHMMSS` before writing. Atomic write via Python's `os.replace`. Refuses to register a broken entry if the lens binary is missing.

### `scripts/uninstall.sh` — clean removal

```
./scripts/uninstall.sh                         # remove skill + lens + MCP entry
./scripts/uninstall.sh --keep-lens             # remove skill + MCP, keep lens binary
./scripts/uninstall.sh --keep-mcp              # remove skill + lens, keep mcpServers.lens
./scripts/uninstall.sh --dest DIR              # custom skills root
./scripts/uninstall.sh --bin-dir DIR           # custom lens bin dir
./scripts/uninstall.sh --claude-json PATH      # custom claude.json
./scripts/uninstall.sh --dry-run               # show what would be removed
./scripts/uninstall.sh --quiet                 # suppress non-error output
```

`uninstall.sh` also reaps orphan staging directories (`.pro-coder.staging.*`) left behind by interrupted prior installs — useful if `install.sh` was SIGKILL'd mid-copy.

### `scripts/test/round_trip.sh` — regression test suite

```
bash scripts/test/round_trip.sh
# 73 assertions: canonicalisation, safe-dest guard, orphan-staging reap
# (incl. dry-run and relative-dest), copy + symlink install/uninstall
# round-trips, install + uninstall idempotency, extended-flag forms
# (--dest=VALUE, --quiet, --dry-run), SKILL.md meta-tests, and the
# --strict allow-list + sc_assert_not_root guards.
# Self-cleaning. Exit 0 on PASS, non-zero with failure count otherwise.
```

### `lens` CLI — full subcommand surface

The bundled lens binary at `~/.claude/bin/lens`. All subcommands run from the project root (or pass an explicit path).

**Index lifecycle:**

```
lens init [PATH] [--no-gitignore]   # create .lens/ + schema; modifies .gitignore unless --no-gitignore
lens index                          # full build from scratch
lens update                         # incremental — re-extract changed files
lens <PATH>                         # graphify-compat: index the given path
lens . --update                     # graphify-compat: incremental update of cwd
lens watch [--debounce MS]          # watch the project and reindex on file changes
```

**Retrieval (read-only, auto-freshness):**

```
lens follow <symbol> [--from FILE:LINE] [--budget N]   # def + doc + signature + body + callers
lens refs <symbol> [--limit N]                          # caller/reference sites
lens query "<question>" [--dfs] [--budget N]            # BFS/DFS over the symbol graph
lens explain <symbol>                                   # plain-language summary
lens path <from> <to>                                   # shortest connection between two symbols
lens slice <file>:<line> [--budget N]                   # minimal context around a location
lens map [--scope DIR] [--depth N]                      # architecture summary
```

**Telemetry:**

```
lens meter                                              # current input/output token tally
lens meter --json                                       # JSON output
lens meter --diff                                       # delta since last call
lens meter --since 1h                                   # delta over last hour
lens meter --reset                                      # zero the counters
lens meter --record-input N --record-output M           # incremental record (called by harnesses)
```

**Auxiliary:**

```
lens add <url>                                          # fetch http(s)/file:// to .lens/raw/, index if recognised
lens mcp                                                # run as a stdio MCP server (used by ~/.claude.json)
```

**Environment variables:**

| Variable | Default | Effect |
|---|---|---|
| `LENS_NO_AUTO_UPDATE` | unset | Set to `1` to disable auto-freshness for a session. |
| `LENS_FRESHNESS_THROTTLE_SECONDS` | `5` | Tune the auto-freshness throttle window. |

---

## Lens — token-efficient retrieval

`lens` is a Rust CLI vendored at `lens/` in this repo. It builds a SQLite-backed map of definitions, references, calls, imports, and type relationships across your codebase, then exposes the high-leverage retrieval verbs listed above.

| Verb | What it returns | Token cost |
|---|---|---|
| `lens query "<topic>" --budget 2000` | Symbol-graph seeds for a topic with `file:line` anchors | ≤ budget |
| `lens follow <symbol> --budget 1500` | Doc comment + definition + signature + body + caller list + language tag — the "Ctrl+Click" primitive | ≤ budget |
| `lens refs <symbol> --limit 20` | Caller and reference sites with file:line — for impact analysis | small |
| `lens explain <symbol>` | Plain-language summary of a symbol and its neighbours | small |
| `lens path "A" "B"` | Shortest connection between two symbols | small |
| `lens slice <file>:<line> --budget 1500` | Minimal context around a location | ≤ budget |
| `lens map [--scope DIR] [--depth N]` | Architecture summary by directory with hot-spot ranking | medium |
| `lens meter [--json] [--diff] [--reset]` | Persistent token counters across sessions / `/clear` | tiny |
| `lens watch [--debounce MS]` | Auto-reindex on file changes; long-running | (server mode) |
| `lens add <url>` | Fetch + index a remote source file | n/a |
| `lens mcp` | Stdio MCP server — Claude Code calls verbs as tools, no Bash | (server mode) |
| `lens . --update` | Incremental re-index of changed files (graphify-compat alias) | n/a |

The win versus `Read` + `Grep`: `lens follow some_function --budget 1500` returns ~1500 tokens regardless of how big the source file is. `Read` on a 2000-line file returns ~50k tokens. Across the per-task super-qa loops (which re-traverse the blast radius repeatedly), the savings compound.

**Doc comments come first.** Per-language extractors harvest `///` (Rust), docstrings (Python), JSDoc / `//` (TS/JS), and `//` (Go) at index time. `lens follow` surfaces them as a `> blockquote` ahead of signature and body — for well-documented code Claude often doesn't need the body at all.

**Cross-language disambiguation.** When `lens follow Foo` matches symbols in multiple languages, the ambiguity message tags each candidate with its language and explicitly flags "cross-language: rust, python, go". The resolved follow output also names the language so Claude knows which language slice it received. Use `--from FILE:LINE` to disambiguate.

**Auto-freshness.** Every read-mode lens command (`follow`/`refs`/`query`/`explain`/`path`/`slice`/`map`) checks for file drift and runs an incremental update if anything changed since last index. Throttled (default 5 s window via `.lens/freshness.txt`) so a flurry of lens calls doesn't repeatedly walk the tree. Opt out: `LENS_NO_AUTO_UPDATE=1`. Tune: `LENS_FRESHNESS_THROTTLE_SECONDS=N`.

**MCP mode** (`lens mcp`) is the deepest integration: Claude Code calls each lens verb as a structured tool with JSON Schema input contracts, response-as-content-blocks, no Bash boilerplate. Auto-wired into `~/.claude.json` by `./scripts/install.sh` (pass `--no-mcp` to skip).

Lens is open source at [github.com/sudeep-dasgupta/lens](https://github.com/sudeep-dasgupta/lens). The version shipped here is pinned in `lens/VENDOR.txt`.

---

## How it works — two agents, one team

```
              ┌─────────────────────────────┐
              │      pro-coder              │
              │  (architect + implementer)  │
              └──────────────┬──────────────┘
                             │  task complete →
                             ▼
                    ┌────────────────────┐
                    │  spawn super-qa    │  ← fresh isolated context
                    │  (Agent tool)      │     (no memory of coder)
                    └─────────┬──────────┘
                              │
              ┌───────────────▼────────────────┐
              │  super-qa runs lens follow/refs│
              │  reads diff, runs tests,       │
              │  adversarially probes          │
              └───────────────┬────────────────┘
                              │
                  ┌───────────┴────────────┐
                  ▼                        ▼
          VERDICT: PASS              VERDICT: FAIL
          (zero BLOCKER,             (BLOCKER / MAJOR /
           zero MAJOR)                MINOR defects with
                  │                   file:line + repro)
                  │                        │
        advance to next task               │
                                           ▼
                              pro-coder fixes,
                              re-spawns super-qa
                              with "Previous failures
                              addressed: ..."
                                           │
                                           └──── loops until PASS
```

### The split

| | pro-coder | super-qa |
|---|---|---|
| **Role** | Architect, implementer | Adversarial reviewer |
| **Context** | Persistent across the session | Fresh per spawn (`/clear` equivalent) |
| **Writes code?** | Yes | **Never** — read-only |
| **Trust level** | Earned by passing QA | Default skeptic |
| **Output** | Code, tests, plan | Structured verdict (PASS/FAIL + tiered defects) |

### The loop

- **Unbounded** — runs as many rounds as needed.
- **Severity-tiered** — PASS requires zero `BLOCKER` and zero `MAJOR` defects. `MINOR` defects are logged but don't block.
- **Stuck-loop detection** — if the *same defect* (same `file:line`, same root cause) recurs twice in a row, the task is treated as misspecified, escalated to the user, and re-decomposed in P3. Prevents livelock on bad specs.
- **Dispute protocol** — pro-coder may challenge a false-positive defect *once per task* with file:line evidence; super-qa adjudicates. More than one dispute per task = halt.

### Two QA gates per section

- **P4.5 — per-task gate.** Every implemented task is reviewed individually before it advances.
- **P5 — section-level gate.** Before section close, super-qa runs *once more* over the cumulative section diff. Catches defects no individual task review can see: composition breaks, overlapping responsibilities, inconsistent public API surface, cross-task perf interactions.

---

## The 6-phase loop (Brainiac-OS v5)

Every code-touching task runs through six phases. The active phase is declared at the top of every response.

| Phase | Name | What happens |
|---|---|---|
| **P1** | Comprehend & Code-map | Bootstrap project state, read `CLAUDE.md`, run `lens query <blast radius>` (or Read/Grep/Glob in fallback mode) |
| **P2** | Research | Read every file in blast radius, cite `file:line`, catalog idioms + failure modes |
| **P3** | Plan | Decompose into atomic tasks (≤100 LOC each, named verifying tests). Wait for ack only on big plans (>5 files, new dep, public API change, CI change) |
| **P4** | Implement & Test | One task at a time. Tests in the same task. Full suite must pass |
| **P4.5** | **Super-QA gate** | Spawn isolated super-qa subagent → adversarial review → loop until `VERDICT: PASS` |
| **P5** | Audit & Re-index | Requirement-traceability matrix, perf audit, **section-level super-qa pass**, `lens . --update` |
| **P6** | Section Boundary | Snapshot to `.claude/state/current_section.md`, propose CLAUDE.md additions, stop → user `/clear`s and starts the next section |

### Hard rules (invariants)

1. `lens query` (or Read/Grep/Glob fallback) opens every code task. `lens . --update` closes it.
2. Section boundaries (P6) are mandatory between sections. No two sections share one context.
3. **Never write to `CLAUDE.md` directly.** Proposals go to `.claude/state/claude_md_proposals.md`. The user owns the project contract.
4. No implementation without a presented plan.
5. Tests live in the same task as the code, not later.
6. Every non-trivial task is gated by super-qa. No `[x]` without a `PASS` verdict.
7. Read files before writing — never assume contents from memory.
8. No `unwrap()` / `expect()` / `panic!()` on production paths.
9. No blocking calls inside async functions.
10. No `Mutex` on declared hot paths — lock-free, sharded, or atomic.
11. Match existing project style; surrounding code is the style guide.
12. One concern per task. If it grows, split.
13. No trailing summaries of what was just done. The diff speaks for itself.

### How many rounds will super-qa and pro-coder loop?

**No fixed cap.** Loop is unbounded by design.

| Task type | Typical rounds to PASS |
|---|---|
| Trivial (skips QA via fast-path) | 0 |
| Normal task | 2–4 |
| Hard / cross-cutting task | 5–8 |
| >10 rounds | rare — stuck-loop should fire by then |

The only forced exits:

1. **PASS** — zero BLOCKER, zero MAJOR. Task closes.
2. **Stuck-loop detection** — same defect (same file:line, same root cause) twice in a row → escalate, treat as misspecified, return to P3 and re-decompose.
3. **Dispute abuse** — pro-coder may challenge a false-positive defect *once per task* with file:line evidence. A second dispute halts and escalates.

A hard cap (e.g. "3 rounds then ship") would let defects through. An infinite loop without stuck-detection would let bad specs spin forever. The combination gives the QA pass real teeth without the risk of livelock.

---

## Examples

Concrete sample prompts live in [`examples/`](examples/README.md). Each shows one shape of work the skill handles well — Rust feature with a perf budget, async race-condition fix, cross-language refactor, fast-path docs change, and the section-boundary protocol. Each example pairs the prompt with what to expect and an annotation explaining what the skill does internally (why it spawns super-qa, why it writes to `.claude/state/`, why it picks lens over Read).

---

## Update

```bash
cd ~/code/claude-skill && git pull
./scripts/install.sh --force        # rebuild + reinstall
```

For symlink installs (`./scripts/install.sh --symlink`), the skill source updates automatically on `git pull`. Lens still rebuilds when its source hash changes (fast — incremental cargo build).

After updating, restart Claude Code so it picks up any changed MCP server config.

---

## Uninstall

```bash
cd ~/code/claude-skill
./scripts/uninstall.sh              # remove skill + lens binary + mcpServers.lens entry
./scripts/uninstall.sh --keep-lens  # remove skill + MCP, keep the lens binary
./scripts/uninstall.sh --keep-mcp   # remove skill + lens, keep the claude.json entry
./scripts/uninstall.sh --dry-run    # show what would be removed without removing
```

Manual uninstall:

```bash
rm -rf ~/.claude/skills/pro-coder ~/.claude/bin/lens ~/.claude/bin/.lens.installed.sha
# Then edit ~/.claude.json and remove the "lens" key under "mcpServers".
```

The script refuses to operate on system paths or `$HOME` directly — same safe-dest guard as install.sh, with `..`-traversal canonicalisation.

---

## Tests

The repo ships with a committed regression suite at `scripts/test/round_trip.sh` covering the install pipeline.

```bash
bash scripts/test/round_trip.sh
```

Sub-tests (73 assertions total):

| Group | What it covers |
|---|---|
| `test_canonicalize` (13) | `sc_canonicalize_dest` — absolute / trailing-slash / `..` bypass / `//` collapse / relative against `$PWD` / clamp at root / empty input |
| `test_safe_dest_guard` (8) | `sc_assert_safe_dest` accepts legitimate paths; rejects HOME / `/` / system paths / `..`-traversal bypasses |
| `test_orphan_reap` (7) | `uninstall.sh` reaps `.pro-coder.staging.*` orphans incl. dry-run, relative-dest, unrelated-dir preservation |
| `test_round_trip_copy` (10) | install --copy → assert → uninstall → assert; install + uninstall idempotency |
| `test_round_trip_symlink` (11) | install --symlink → assert → uninstall → assert; symlink target verification; install + uninstall idempotency |
| `test_install_extended_flags` (12) | `install.sh --dest=VALUE` and `--quiet` and `--dry-run`; `uninstall.sh --dest=VALUE`; empty `--flag=` rejection |
| `test_skill_meta` (1, delegates to `skill_meta.sh` for 27 sub-checks) | SKILL.md frontmatter, required sections, P-reference resolution, markdown code-fence balance, no placeholder leaks |
| `test_strict_and_root_guards` (11) | `sc_assert_strict_allowed` accepts paths under `~/.claude/`, rejects others incl. `..`-traversal; `sc_assert_not_root` refuses EUID=0 by default and accepts under `--allow-root`; end-to-end `install.sh --strict` |

The suite is **self-cleaning** (single shared parent jail under `/tmp`, single `rm -rf` at exit), **CI-friendly** (exit 0 on PASS, non-zero with numeric failure count otherwise), and **cwd-independent** (passes from any directory).

---

## Troubleshooting

**`/pro-coder` doesn't appear as a slash command in Claude Code.**
- Confirm `~/.claude/skills/pro-coder/SKILL.md` exists and is readable: `ls -la ~/.claude/skills/pro-coder/SKILL.md`
- Restart Claude Code — skills are scanned at startup.
- Run `claude --help` to confirm the CLI version supports skills (you need a recent Claude Code version).

**`cargo build` fails during install.**
- Ensure you have a Rust toolchain: `cargo --version` should print `cargo 1.85+`.
- If you see `rustup could not choose a version of cargo to run`, run `rustup default stable`.
- If the build fails for another reason, the error is printed in full. Open an issue with the last 40 lines.
- You can always `./scripts/install.sh --no-lens` to install just the skill and use fallback mode.

**`lens: command not found` when I run it directly.**
- Check `~/.claude/bin` is on `$PATH`: `echo $PATH | tr ':' '\n' | grep claude`
- If not, add `export PATH="$HOME/.claude/bin:$PATH"` to your shell rc and reload.
- Or invoke with the full path: `~/.claude/bin/lens --version`.
- For the skill itself, PATH doesn't matter — Claude Code launches lens by absolute path via the MCP entry in `~/.claude.json`.

**MCP wire-up failed (`install-mcp.sh: python3 not found on PATH`).**
- Install Python 3 (`brew install python3` on macOS / your distro's package manager on Linux) and re-run `./scripts/install-mcp.sh`.
- Or wire it manually: open `~/.claude.json`, add the snippet shown by `install-mcp.sh` to `"mcpServers"`, save.
- Without MCP, lens still works via Bash — Claude calls it with `Bash`-tool instead of as a structured tool. The skill detects either path and adapts.

**Claude doesn't seem to be calling lens tools (just runs Bash).**
- Confirm the entry exists: `grep -A2 '"lens"' ~/.claude.json` — should show `command: <path-to-lens>` and `args: ["mcp"]`.
- Confirm the binary is executable: `[ -x ~/.claude/bin/lens ] && echo OK || echo MISSING`.
- Restart Claude Code — MCP servers are spawned at startup, not hot-reloaded.

**Lens reports `0 symbols` indexed.**
- This is expected for non-Rust/Python/TypeScript/JavaScript/Go codebases. The skill detects it at bootstrap and falls back to `Read`/`Grep`/`Glob` for the rest of the session.
- If your project IS in a supported language and you still see 0 symbols, run `lens index` manually and check `.lens/index.db` exists. File an issue at the [lens repo](https://github.com/sudeep-dasgupta/lens) with the project structure.

**Install script refuses to run with "refusing to operate on system path".**
- You probably passed `--dest /` or `--dest /usr` or similar — or `--dest ~/skills/../../../etc` (the canonicalised path is what gets checked). Pass a writable subdirectory: `--dest ~/.claude/skills` (the default) or `--dest /tmp/test-skills`.
- macOS users: `${TMPDIR}` resolves to `/var/folders/...` which is correctly under the `/var/*` deny rule. Tests use `/tmp` directly (which on macOS symlinks to `/private/tmp`, but the canonicaliser is textual so the `/tmp/...` form is preserved).

**The skill keeps re-running super-qa and never PASSes.**
- Check the verdicts — if super-qa returns the *same defect* (same `file:line`, same root cause) twice in a row, the skill's stuck-loop detection should fire and escalate to you. If it doesn't, your spec is ambiguous — refine the prompt and start a fresh section.

**`bash scripts/test/round_trip.sh` fails on a fresh clone.**
- Ensure you're running from the repo root (the script uses `BASH_SOURCE`-derived paths so it tolerates other cwds, but the `${repo_root}/pro-coder/SKILL.md` lookup needs the file to exist).
- Check `cargo` is NOT what's failing — the test passes `--no-lens` to install.sh so cargo isn't invoked. If it does fail with a cargo error, that's a regression; file an issue.
- Read the failure line — each FAIL prints the expected vs actual so the diagnosis is in the output.

---

## Customisation

The skill is one file (`pro-coder/SKILL.md`) — plain Markdown with YAML frontmatter. Fork, edit, reinstall.

Common edits:

- **Swap the language defaults** in the Identity section if your stack isn't Rust / Python.
- **Adjust the section-boundary threshold** (default: 5+ tasks) in the P6 triggers.
- **Swap `lens` for a different code-graph tool** by find-and-replacing the verbs in `pro-coder/SKILL.md` (`lens query`, `lens follow`, `lens . --update`). Pass `--no-lens` to `install.sh` if you have your own tool already on `$PATH`.
- **Change the super-qa subagent type** (default: `general-purpose`) if you have a more specific QA agent registered.
- **Tighten or relax severity tiers** in the P4.5 verdict format.

After editing, re-run `./scripts/install.sh --force` (or `git pull && ./scripts/install.sh --force` if you're tracking upstream).

---

## FAQ

**Q: Does this work without `lens`?**
Yes, in fallback mode. SKILL.md detects whether `lens` is on `$PATH` (and whether `lens index` produced any symbols) once per project at bootstrap. If lens is absent or the project has 0 symbols indexed (lens supports Rust, Python, TypeScript, JavaScript, and Go today), the skill swaps `lens query`/`lens follow` for `Read`/`Grep`/`Glob` automatically. The loop still runs end-to-end; the difference is per-call token cost — lens slices are budget-capped (`--budget 1500` returns ~1500 tokens), `Read`/`Grep` on a large file isn't.

**Q: What does lens actually buy me?**
Symbol-aware, budget-capped slices. `lens follow some_function --budget 1500` returns the definition + signature + body + caller list in ~1500 tokens, regardless of how big the file is. `lens query "auth middleware" --budget 2000` returns the symbol-graph seeds for that topic with `file:line` anchors. By contrast, `Read` on a 2000-line file returns ~50k tokens and `Grep` returns line matches without structural context. Token savings compound across the per-task and section-level super-qa loops, where the same blast radius gets re-traversed multiple times.

**Q: Does Claude Code launch lens itself, or do I have to keep it running?**
Claude Code launches lens automatically as an MCP stdio server at startup, via the `mcpServers.lens` entry in `~/.claude.json`. You don't keep anything running. The binary lifecycle is owned by Claude Code.

**Q: Can I disable super-qa for fast iteration?**
The fast-path mode skips P3 plan presentation and P6, but still runs P4 tests and P5 `lens . --update` (in lens mode). It also skips super-qa — but only for genuinely trivial changes (typo, single-line rename, doc tweak). If a "trivial" change touches behaviour, the agent will detect that and switch back to the full loop with super-qa enabled.

**Q: Why is super-qa read-only?**
Separation of concerns. If super-qa could write code, it would patch its own findings, contaminating the artifact under review. The author of the fix should not also be the verifier — that's the whole point of an independent QA pass.

**Q: What if super-qa is wrong about a defect?**
Use the dispute protocol. Super-coder re-spawns super-qa with a `Disputed:` block containing file:line evidence proving the defect doesn't exist (e.g. citing the test that already covers it). Super-qa adjudicates: either withdraws the defect or restates it sharper. Disputes are limited to one per task; abuse halts the loop.

**Q: Will it modify my `CLAUDE.md`?**
Never. `CLAUDE.md` is read-only to the agent. Any suggested additions are appended to `.claude/state/claude_md_proposals.md` for you to review and copy in by hand. You own the project contract.

**Q: How do I see what super-qa actually said?**
The Agent tool surfaces the subagent's report inline. Super-coder also records the one-line PASS summary in the task checklist (`[x] T2 — qa: <summary>`). For deeper inspection, the structured verdict format (PASS/FAIL + BLOCKER/MAJOR/MINOR with file:line + repro) is in the response stream.

**Q: How do I uninstall the lens binary but keep the skill?**
`./scripts/uninstall.sh --keep-mcp --dest /dev/null` won't work (the safe-dest guard forbids it). Instead, remove just the binary manually: `rm ~/.claude/bin/lens ~/.claude/bin/.lens.installed.sha`, then `./scripts/install-mcp.sh --remove` to clean up the now-broken MCP entry. The skill stays installed and detects the missing lens at next invocation, falling back automatically.

**Q: Can I use this on a TypeScript / Go / C++ project?**
Yes for TypeScript and Go (lens indexes both). For other languages (Java, C++, Ruby, etc.) the skill detects the empty index at bootstrap and switches to fallback mode (`Read` / `Grep` / `Glob`). The loop still runs — you just lose the token-efficient retrieval. More languages are tracked at the [lens repo](https://github.com/sudeep-dasgupta/lens).

**Q: Where do I look if something seems off?**
- `<project>/.claude/state/current_section.md` — what the agent thinks it just did.
- `<project>/.claude/state/code-map/*.md` — what the agent believes about each area of code.
- `<project>/.claude/state/claude_md_proposals.md` — what the agent thinks should be in your project contract.
- `~/.claude/agent-memory/brainiac-os/MEMORY.md` — index of what the agent remembers across sessions.
- `~/.claude.json` — MCP wire-up; the `mcpServers.lens` key is the structured-tool entry point.

---

## License

MIT — see [LICENSE](LICENSE).

---

## Acknowledgements

Built on the **Brainiac-OS v5** system prompt (code-map-first / code-map-last workflow with section-boundary context resets) extended with the super-qa adversarial-review loop, and powered by [`lens`](https://github.com/sudeep-dasgupta/lens) — a symbol-aware Rust CLI vendored under `lens/` — for budget-capped, structural code retrieval.
