# super-coder

> A Claude Code skill that turns Claude into a two-agent engineering team — a hyper-rigorous **architect** that plans and implements, and an adversarial **QA reviewer** that gates every task. They iterate until the work is provably correct, not just plausibly correct.

Invoke with `/super-coder` inside Claude Code.

---

## Why

Default Claude is a generalist. It writes plausible code, declares victory, and moves on. For real engineering work — distributed systems, performance-critical paths, multi-component refactors — you want something that:

- Maps the blast radius **before** it touches anything.
- Plans atomically, implements one concern at a time, and writes tests in the same breath as the code.
- Refuses to mark a task done until an **independent reviewer** with no memory of the implementation has hammered on it and signed off.
- Resets its own context window before it starts hallucinating from accumulated assumptions.
- Treats your `CLAUDE.md` as read-only and proposes changes through a queue you approve by hand.

That's what this skill is.

---

## How it works — two agents, one team

```
              ┌─────────────────────────────┐
              │      super-coder            │
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
              │  super-qa runs graphify query, │
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
                              super-coder fixes,
                              re-spawns super-qa
                              with "Previous failures
                              addressed: ..."
                                           │
                                           └──── loops until PASS
```

### The split

| | super-coder | super-qa |
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
- **Dispute protocol** — super-coder may challenge a false-positive defect *once per task* with file:line evidence; super-qa adjudicates. More than one dispute per task = halt.

### Two QA gates per section

- **P4.5 — per-task gate.** Every implemented task is reviewed individually before it advances.
- **P5 — section-level gate.** Before section close, super-qa runs *once more* over the cumulative section diff. Catches defects no individual task review can see: composition breaks, overlapping responsibilities, inconsistent public API surface, cross-task perf interactions.

---

## The 6-phase loop (Brainiac-OS v5)

Every code-touching task runs through six phases. The active phase is declared at the top of every response.

| Phase | Name | What happens |
|---|---|---|
| **P1** | Comprehend & Graphify | Bootstrap project state, read `CLAUDE.md`, run `graphify query <blast radius>` |
| **P2** | Research | Read every file in blast radius, cite `file:line`, catalog idioms + failure modes |
| **P3** | Plan | Decompose into atomic tasks (≤100 LOC each, named verifying tests). Wait for ack only on big plans (>5 files, new dep, public API change, CI change) |
| **P4** | Implement & Test | One task at a time. Tests in the same task. Full suite must pass |
| **P4.5** | **Super-QA gate** | Spawn isolated super-qa subagent → adversarial review → loop until `VERDICT: PASS` |
| **P5** | Audit & Graphify | Requirement-traceability matrix, perf audit, **section-level super-qa pass**, `graphify . --update` |
| **P6** | Section Boundary | Snapshot to `.claude/state/current_section.md`, propose CLAUDE.md additions, stop → user `/clear`s and starts the next section |

### Hard rules (invariants)

1. `graphify query` opens every code task. `graphify . --update` closes it.
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

---

## Installation

### Prerequisites

- **Claude Code CLI** installed (`claude` available in your shell). See [Anthropic's install docs](https://docs.anthropic.com/en/docs/claude-code/quickstart).
- **`graphify` skill** (or an equivalent tool exposing `graphify query <args>` and `graphify . --update`). The P1/P5 phases call it directly. Without it, install will succeed but the loop's invariants will fail at runtime.

### Step 1 — Clone the repo

```bash
git clone https://github.com/<your-username>/claude-skill.git ~/code/claude-skill
```

(Replace `<your-username>` with the GitHub user/org once you've pushed.)

### Step 2 — Install the skill

You have two options. Pick one.

#### Option A — Copy (recommended for end-users)

```bash
mkdir -p ~/.claude/skills
cp -r ~/code/claude-skill/super-coder ~/.claude/skills/
```

#### Option B — Symlink (recommended for skill developers)

```bash
mkdir -p ~/.claude/skills
ln -s ~/code/claude-skill/super-coder ~/.claude/skills/super-coder
```

A symlink lets you `git pull` upstream improvements without re-copying.

### Step 3 — Verify

The skill lives at `~/.claude/skills/super-coder/SKILL.md`. Confirm:

```bash
ls -la ~/.claude/skills/super-coder/
# expected: SKILL.md
```

Open Claude Code in any project directory:

```bash
cd ~/your-project
claude
```

Type `/super-coder` — Claude should acknowledge the skill is active and ask for an engineering objective.

### Updating

If you cloned to `~/code/claude-skill`:

```bash
cd ~/code/claude-skill && git pull
# if you used Option A (copy):
cp -r ~/code/claude-skill/super-coder ~/.claude/skills/
# if you used Option B (symlink): nothing — symlink picks up the change automatically
```

### Uninstalling

```bash
rm -rf ~/.claude/skills/super-coder
```

---

## Usage

### Trigger forms

```
/super-coder build a sharded connection pool with backpressure for our Tokio service
```

```
/super-coder the ingest pipeline drops messages under load — find the cause and fix it
```

```
/super-coder refactor src/auth/session.rs to remove the Mutex on the hot path
```

### What happens after you send a prompt

1. **Bootstrap** (once per project) — creates `.claude/state/`, asks once whether to gitignore it (the answer is remembered in `.claude/state/gitignore_policy`), checks for `CLAUDE.md`.
2. **P1** — restates the goal, reads `CLAUDE.md` and any prior section snapshot, runs `graphify query` on the blast radius.
3. **P2 → P5** — research with `file:line` citations, atomic task plan, implementation task-by-task with tests, **per-task super-qa gate** until PASS, **section-level super-qa pass**, requirement-traceability matrix, `graphify . --update`.
4. **P6** (5+ tasks done or you say `section boundary`) — snapshots to `.claude/state/current_section.md`, appends any CLAUDE.md proposals, stops. You run `/clear` and re-invoke `/super-coder` with the next section's prompt.

### Fast path

Trivial changes (typo, single-line rename, doc tweak, formatting only) take a **fast path** — still runs `graphify query` / `graphify . --update` and tests, but skips the plan presentation and skips P6. If the change grows beyond trivial mid-implementation, the agent stops and switches back to the full loop.

### When to start a new section

Either:
- The agent triggers P6 itself (5+ tasks completed, or it senses context drift).
- You explicitly type `section boundary` or `reset`.

After P6, **run `/clear` before the next prompt** — the snapshot on disk preserves what matters; the conversation context does not.

---

## What the skill writes to disk

| Path | Purpose | Lifetime | Owner |
|---|---|---|---|
| `.claude/state/gitignore_policy` | One-line marker (`ignore` or `commit`) so the bootstrap question is asked once per project | Project | agent |
| `.claude/state/current_section.md` | Snapshot at section boundary — verified facts, open invariants, next-section blast radius | Overwritten each P6 | agent |
| `.claude/state/claude_md_proposals.md` | Append-only queue of suggested `CLAUDE.md` additions for **your** review | Project | agent (write) / user (review) |
| `CLAUDE.md` | Project contract / conventions | Project lifetime | **user only** — agent has read-only access |
| `~/.claude/agent-memory/brainiac-os/` | Cross-session durable memory (user prefs, validated feedback, stakeholder context) | Across all sessions | agent + user |

The four persistence layers — conversation context, section state, project contract, agent memory — are kept strictly separate. See `super-coder/SKILL.md` for the full table and rationale.

---

## How many rounds will super-qa and super-coder loop?

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
3. **Dispute abuse** — super-coder may challenge a false-positive defect *once per task* with file:line evidence. A second dispute halts and escalates.

A hard cap (e.g. "3 rounds then ship") would let defects through. An infinite loop without stuck-detection would let bad specs spin forever. The combination gives the QA pass real teeth without the risk of livelock.

---

## Customisation

The skill is one file (`super-coder/SKILL.md`) — plain Markdown with YAML frontmatter. Fork, edit, reinstall.

Common edits:

- **Swap the language defaults** in the Identity section if your stack isn't Rust / Python.
- **Adjust the section-boundary threshold** (default: 5+ tasks) in the P6 triggers.
- **Replace `graphify`** with a different code-graph tool by find-and-replacing the command.
- **Change the super-qa subagent type** (default: `general-purpose`) if you have a more specific QA agent registered.
- **Tighten or relax severity tiers** in the P4.5 verdict format.

---

## FAQ

**Q: Does this work without the `graphify` skill?**
Not as designed — `graphify query` and `graphify . --update` are mandatory at P1 and P5. You can fork the skill and replace those calls with `Grep` + `Glob` for a degraded but functional version. Mapping the blast radius before reading is the point of the entry phase; without it, the agent reads files in arbitrary order and misses dependencies.

**Q: Can I disable super-qa for fast iteration?**
The fast-path mode skips P3 plan presentation and P6, but still runs P4 tests and P5 graphify update. It also skips super-qa — but only for genuinely trivial changes (typo, single-line rename, doc tweak). If a "trivial" change touches behaviour, the agent will detect that and switch back to the full loop with super-qa enabled.

**Q: Why is super-qa read-only?**
Separation of concerns. If super-qa could write code, it would patch its own findings, contaminating the artifact under review. The author of the fix should not also be the verifier — that's the whole point of an independent QA pass.

**Q: What if super-qa is wrong about a defect?**
Use the dispute protocol. Super-coder re-spawns super-qa with a `Disputed:` block containing file:line evidence proving the defect doesn't exist (e.g. citing the test that already covers it). Super-qa adjudicates: either withdraws the defect or restates it sharper. Disputes are limited to one per task; abuse halts the loop.

**Q: Will it modify my `CLAUDE.md`?**
Never. `CLAUDE.md` is read-only to the agent. Any suggested additions are appended to `.claude/state/claude_md_proposals.md` for you to review and copy in by hand. You own the project contract.

**Q: How do I see what super-qa actually said?**
The Agent tool surfaces the subagent's report inline. Super-coder also records the one-line PASS summary in the task checklist (`[x] T2 — qa: <summary>`). For deeper inspection, the structured verdict format (PASS/FAIL + BLOCKER/MAJOR/MINOR with file:line + repro) is in the response stream.

---

## License

MIT — see [LICENSE](LICENSE).

---

## Acknowledgements

Built on the **Brainiac-OS v5** system prompt (graphify-first/graphify-last workflow with section-boundary context resets) extended with the super-qa adversarial-review loop.
