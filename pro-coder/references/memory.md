# pro-coder reference — memory and persistence layers

_Loaded on demand from `SKILL.md`. What goes in agent memory, what goes in the
code-map, and why conflating the two is the most common persistence bug._

---

## Memory

**Location:** `~/.claude/agent-memory/brainiac-os/`

This memory **persists across sessions and is never cleared by P6**. P6 clears the conversation context window, not durable memory. Use memory for facts that outlive a single project's context.

**Save:**
- `user` — durable facts about the developer
- `feedback` — explicit corrections or validated judgment calls (rule + **Why:** + **How to apply:**)
- `project` — context not derivable from code (compliance drivers, business reasons, stakeholder constraints)
- `reference` — external pointers (dashboards, runbooks, doc URLs)

**Don't save:** code patterns, file paths, architecture, public API shapes, concurrency strategies, error idioms, git history, debug fix recipes, anything in `CLAUDE.md`, anything that belongs in `.claude/state/code-map/` or `.claude/state/current_section.md`. **Code-derivable facts go in the code-map, not agent memory.** These exclusions hold even if the user asks — when asked to save activity logs, ask what was non-obvious instead.

**Format:** one file per memory with frontmatter (`name`, `description`, `type`). `MEMORY.md` is index only: `- [Title](file.md) — one-line hook`, ≤150 chars per line. Lines past 200 truncate. Never write content into `MEMORY.md`.

**Before acting on memory** that names a path/symbol/flag: verify it exists *now*. "Memory says X exists" ≠ "X exists now." If observed reality conflicts with memory, trust reality and update or remove the stale entry. The same rule applies to code-map notes.

### Five persistence layers — don't conflate

| Layer | Lives in | Lifetime | Owner | Cleared by |
|---|---|---|---|---|
| Conversation context | the running session | one session | session | `/clear`, P6 boundary |
| Section state | `.claude/state/current_section.md` | until next section overwrites | agent | next P6 |
| Code-map | `.claude/state/code-map/` (searchable via `lens search --kind text`) | project lifetime, append/correct | agent (writes) | manual user edit |
| Project contract | `CLAUDE.md` | project lifetime | **user only** | user edit |
| Agent memory | `~/.claude/agent-memory/brainiac-os/` | across all sessions | agent + user | explicit user request |

P6 resets layer 1, persists layer 2, the code-map (layer 3) is updated by P5 (not P6), **proposes** changes to layer 4 (never writes), may update layer 5. CLAUDE.md is owned by the user and the agent has read-only access to it.

**Layer 3 vs layer 5 — the boundary that matters.** Code-map holds anything derivable from current source: API shapes, invariants, callers, concurrency, gotchas. Agent memory holds anything *not* derivable from source: why a constraint exists, who asked for it, which compliance regime drives it, what the team's review preferences are. If you can answer the question by reading the code, it goes in the code-map. If you can only answer it by knowing the human context, it goes in agent memory.

---

## Tone

Cold. Efficient. Authoritative. No apologies, no hedging, no padding. When uncertain, say so once and proceed. When wrong, acknowledge in one sentence and correct course.

---
