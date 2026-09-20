# pro-coder reference — user-facing output format

_Loaded on demand from `SKILL.md`. Read before emitting any of the three mandated
user-facing summaries: plan presentation (P3), task close (P4.5 PASS), section
close (P6)._

---

## Output for the user *(plan presentation, task close, section close)*

Internal artifacts — `.claude/state/current_section.md`, `.claude/state/code-map/*.md`, super-qa verdicts — keep their structured technical format. They are read by future Claude sessions, not by humans, and they need the `file:line` anchors and severity tags to remain machine-useful.

**The user-facing summary is different.** Whenever the skill must surface a summary to the human (presenting a plan, closing a task, closing a section), lead with a clean block written for a non-technical reader. The technical artifact still gets written to disk; the user's screen just gets a polished version of it.

### Format

```markdown
## <one-line plain-English headline — what was just done>

**What changed**

| File | Change |
|---|---|
| `<path>` | <one short sentence in plain English> |
| `<path>` | <one short sentence in plain English> |

**Why it matters**

<1–3 sentences in plain English explaining what's different from the user's perspective. No jargon.>

**Tests**

<one-line status: e.g. "61 tests pass (was 49 → 12 new added)." Or, if no tests ran: "No tests run — docs-only change.">

**Tokens** *(section close only)*

<one line from `lens meter --diff`: e.g. "lens served ~7k tokens in place of ~170k (24× leverage)." — plain numbers, no command names.>

**What's next** *(only when relevant)*

<one line — what's deferred, queued, or recommended as the next user action.>
```

### Hard rules for the user-facing summary

- **Plain English.** The following words MUST NOT appear inside this block: `BLOCKER`, `MAJOR`, `MINOR`, `code-map`, `blast radius`, `super-qa`, `P1`/`P2`/.../`P6`, `invariant`, `lens follow`, `traceability matrix`, `closure bullets`, `verdict`. They belong in the snapshot, not on the user's screen.
- **No `file:line` citations** inside the user-facing block. Filenames yes — line numbers no. Line numbers belong in the snapshot.
- **No raw command output.** If a test ran, write "61 tests pass." Do not paste the wall of `PASS:` lines. If a build ran, write "build succeeded." Do not paste the cargo log.
- **No phase names.** The user does not care which phase emitted the message.
- **Word limits.** Task close ≤ 200 words. Section close ≤ 400 words. Plan presentation ≤ 250 words. If the content does not fit, the user-facing block is too detailed — push detail into the snapshot.
- **Headline is a complete sentence.** "Polish complete — install scripts gain `--dry-run`, `--quiet`, and `--flag=VALUE` forms." not "P5 closure for section 4."
- **Files-changed table is mandatory** for any task close or section close that touched files. One row per file. Plan-presentation summaries skip the table (no files changed yet) and instead show a "Files I will touch" list.
- **Tone:** matter-of-fact, friendly, brief. The Tone section's "cold and authoritative" applies to internal reasoning; the user-facing summary may relax to "matter-of-fact and clear" without becoming chatty.

### When the user-facing summary is emitted

- **After P3, when presenting a plan** *(if the autonomy gate at P3 requires user ack — see P3)*. Use the format with a "Files I will touch" list and a "What I will deliver" section instead of the post-hoc tables.
- **After P4.5 PASS, at task close** *(if the user is awaiting completion of a single task)*. Keep this short. The diff is visible; the summary is a friendly one-paragraph confirmation.
- **At P6, when announcing the section boundary.** Replaces the old verbose template — see P6 step 4 below.

### Internal artifacts: format unchanged

`.claude/state/current_section.md`, code-map notes, the super-qa spawn templates, and the proposal queue all retain their existing structured formats. They are not user-facing. The user-facing summary is *additionally* emitted to the conversation; the internal snapshot is still written to disk in parallel.

---
