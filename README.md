# Super-Coder for Claude CLI

A Brainiac-OS system architect agent for complex engineering tasks. Built on top of Claude Code with a rigorous 6-phase workflow: Comprehend → Research → Plan → Implement → Audit → Boundary.

## Features

- **Graphify-first workflow** — Maps code to knowledge graph before and after every task for targeted context
- **6-phase execution loop** — Structured approach to complex engineering problems
- **Section-boundary resets** — Prevents context hallucination by resetting every 5+ tasks
- **Zero panics on production** — No `unwrap()`/`expect()`/`panic!()` on production paths
- **Test-first implementation** — Tests live in the same task as the code
- **CLAUDE.md proposal workflow** — Never edits project contract directly, only proposes
- **Token-optimized** — Minimizes token consumption via targeted blast-radius mapping and context resets

## Installation

### Option 1: Git Clone

```bash
git clone https://github.com/YOUR_USERNAME/super-coder-claude.git ~/.claude/skills/super-coder
```

### Option 2: Manual Install

```bash
mkdir -p ~/.claude/skills/super-coder
# Copy SKILL.md and INSTRUCTIONS.md to that directory
```

### Option 3: Via Claude CLI

```bash
# From within claude cli
/super-coder
```

## Usage

```bash
# Start claude cli
claude

# Activate super-coder mode
/super-coder

# Then give it any complex engineering task
> Build a distributed rate limiter that handles 100K req/s per node
> Implement a lock-free concurrent data structure in Rust
> Design and build a multi-region failover system
```

## How It Works

### The 6-Phase Loop

```
P1: Comprehend & Graphify → P2: Research → P3: Plan → P4: Implement & Test → P5: Audit & Graphify → P6: Section Boundary
```

| Phase | Description |
|-------|-------------|
| **P1 — Comprehend** | Restate objective, run graphify query, verify memory |
| **P2 — Research** | Read blast radius files, catalog conventions and failure modes |
| **P3 — Plan** | Decompose into atomic tasks (≤100 LOC each), present for ack if >5 files |
| **P4 — Implement** | One task at a time, tests in same task, full suite passes |
| **P5 — Audit** | Traceability matrix, adversarial review, performance audit, graphify update |
| **P6 — Boundary** | Snapshot to disk, propose CLAUDE.md changes, stop for context reset |

### Section Boundaries

After completing a section (3–7 tasks), the agent:
1. Writes snapshot to `.claude/state/current_section.md`
2. Persists verified facts to agent memory
3. Proposes CLAUDE.md changes to `.claude/state/claude_md_proposals.md`
4. Announces boundary and stops

User runs `/clear` and re-invokes with next section's prompt.

### Token Optimization

The skill minimizes token consumption by:
- Graphifying only the blast radius, not entire codebase
- Writing section snapshots to disk (not context window)
- Resetting context every 5 tasks
- Re-reading verified facts from code rather than carrying state
- Using agent memory for cross-session durable facts

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Super-Coder Agent                     │
├─────────────────────────────────────────────────────────┤
│  Bootstrap (once per project)                           │
│  ├── Ensure .claude/state/ exists                       │
│  ├── Check gitignore policy                             │
│  └── Verify CLAUDE.md presence                          │
├─────────────────────────────────────────────────────────┤
│  6-Phase Loop (per section)                            │
│  ├── P1: Graphify query (entry)                        │
│  ├── P2: Deep research                                 │
│  ├── P3: Plan (ask for ack if >5 files)               │
│  ├── P4: Implement & test                              │
│  ├── P5: Graphify update (exit)                        │
│  └── P6: Snapshot & reset                               │
├─────────────────────────────────────────────────────────┤
│  Persistence Layers                                    │
│  ├── Conversation context (session)                    │
│  ├── Section state (disk)                              │
│  ├── Project contract (CLAUDE.md, user-owned)          │
│  └── Agent memory (cross-session)                      │
└─────────────────────────────────────────────────────────┘
```

## Requirements

- Claude CLI installed
- Graphify skill installed (`~/.claude/skills/graphify/`)
- Opus model access (configured in SKILL.md)

## Configuration

The skill uses Opus model by default. To change:

Edit `~/.claude/skills/super-coder/SKILL.md`:

```yaml
model: opus  # or 'sonnet', 'haiku'
```

## Contributing

1. Fork the repo
2. Create a feature branch
3. Make changes with tests
4. Submit PR

## License

Apache License 2.0 — See [LICENSE](LICENSE) file.

---

**Tone:** Cold. Efficient. Authoritative. No fluff.