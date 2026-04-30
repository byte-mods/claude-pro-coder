# Examples — sample prompts for the pro-coder skill

These are concrete prompts you can paste into Claude Code (with the
pro-coder skill installed) to see the 6-phase loop in action. Each
example illustrates one shape of work the skill handles well.

| File | Shape | Why it's interesting |
|---|---|---|
| [`01-rust-feature.md`](01-rust-feature.md) | New Rust feature, performance-budgeted | Shows the full loop: P1 lens-query, P3 plan, P4 implement + test, P4.5 super-qa adversarial review, P5 audit + code-map update |
| [`02-python-bug-fix.md`](02-python-bug-fix.md) | Race condition in async pipeline | Demonstrates super-qa catching a race the author rationalised away |
| [`03-cross-language-refactor.md`](03-cross-language-refactor.md) | Extract shared module across Python + TypeScript | Lens cross-language disambiguation; multi-section plan with user ack gate at P3 |
| [`04-docs-fast-path.md`](04-docs-fast-path.md) | Pure docs change | Fast-path — skips P3 plan presentation and P6 boundary; still runs P1 + P5 |
| [`05-section-boundary.md`](05-section-boundary.md) | Closing one section, opening the next | Shows the P6 user-facing summary format and the resume protocol |

## Reading the examples

Each example file is structured as:

1. **Prompt** — the exact text you'd paste into Claude Code.
2. **What to expect** — the rough shape of the response. Not a full
   transcript; the skill's actual output depends on your codebase.
3. **What the skill is doing internally** — annotation explaining why
   it's calling lens, why it's spawning super-qa, why it's writing to
   `.claude/state/`, etc. Useful if you want to understand the protocol.

## Running them

The prompts assume:

- The pro-coder skill is installed (run `./scripts/install.sh` from
  the repo root).
- `lens` is built and on `$PATH` (the install does this unless you
  passed `--no-lens`). Without lens, the skill drops to fallback mode
  (Read/Grep/Glob) and the prompts still work — just slower.
- The MCP server is registered in `~/.claude.json` (the install does
  this unless you passed `--no-mcp`).

If your project is in a language lens does not index (currently:
languages outside Rust, Python, TypeScript/TSX, JavaScript/JSX, MJS,
CJS, Go, and Dart), the skill auto-detects and uses fallback mode. The
examples still apply; the prompts don't change.
