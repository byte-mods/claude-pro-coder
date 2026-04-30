# Example 5 — Closing one section, opening the next

## Prompt

> section boundary

(or equivalently: `reset`)

## What to expect

The skill enters **P6 — Section Boundary**. Internally:

1. **Snapshot** written to `.claude/state/current_section.md` with the
   structured technical recap (closure summary, code-map updates,
   verified facts carried forward, open invariants for next section,
   next-section goal). This file is for the next session to read; it is
   not for you.
2. **Code-map updates** propagated under `.claude/state/code-map/`.
3. **CLAUDE.md proposals** appended to `.claude/state/claude_md_proposals.md`
   if any project-wide invariants were observed this section. These are
   never auto-applied — you review and merge them manually.
4. **Lens index refreshed** via `lens . --update` so the next session's
   `lens query` / `lens follow` calls see the current diff.

What you see (the user-facing block) is the clean closure summary in the
"Output for the user" format:

```markdown
## <plain-English headline — what this section delivered>

**What changed**

| File | Change |
|---|---|
| `<path>` | <one-line plain English> |
| ...      | ... |

**Why it matters**

<1–3 sentences explaining what's different from your perspective.>

**Tests**

<one-line test status>

**What's next**

- <one or two lines on what's deferred or queued>
- Suggest `/clear` before the next section so the context window starts fresh.
```

After this announcement, the skill **stops**. It does not start the next
section in the same conversation.

## What you do

1. Run `/clear` (or `/compact` if you want to keep some history).
2. Re-invoke with the next section's prompt.

On resume, the skill's **Resume protocol** kicks in:

1. Reads `CLAUDE.md` (the project contract).
2. Reads `.claude/state/current_section.md` (the snapshot — what the
   prior section left as open invariants and verified facts).
3. Reads the code-map notes covering the new blast radius.
4. Verifies any name/symbol/flag claim against current source via lens
   or Read/Grep before acting.

It then declares P1 and proceeds.

## Why this exists

After 5+ tasks in a single context, even verified facts get conflated
with hallucinated ones. A clean window plus a structured snapshot plus
a persistent code-map is more reliable than a long context with
everything in it. Reloading the relevant code-map notes on resume costs
seconds and prevents the "Claude remembered something that isn't true"
class of failures.
