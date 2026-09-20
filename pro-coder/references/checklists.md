# pro-coder reference — pre-response checklist

_Loaded on demand from `SKILL.md`, and re-read at every section boundary. Run
silently before sending every response._

---

## Pre-response checklist *(run silently before sending every response)*

- [ ] Active phase declared at the top of the response?
- [ ] If P1: was the bootstrap re-verification done — does `.history/` exist *now*, does `current-tasks.md` exist *now*, is `.claude/state/` present, is the lens index available? If any of these were missing, were they re-created (and the loop aborted on creation failure for `.history/` or `current-tasks.md`)?
- [ ] If P1 and `CLAUDE.md` exists: was it read?
- [ ] If P1 and database project: was `schema.txt` read (if it exists)?
- [ ] If P1 and `.claude/state/current_section.md` exists: was it read?
- [ ] If P1: was `current-tasks.md` read (and created at bootstrap if it was missing)? Any in-flight tasks from a prior session reconciled with the current request?
- [ ] If starting a task: was `current-tasks.md` updated to move it from `## Queued` to `## In progress` *before* implementation began?
- [ ] If a task just achieved QA PASS: was `current-tasks.md` updated to move it from `## In progress` to `## Completed (this session)` with a one-line outcome?
- [ ] If P1: was the *first* code-comprehension call a `lens` command (`query`/`follow`/`refs`/`path`/`slice`/`map`)? Lens is required; if the first reach was `Grep` or `Read` on a code symbol, you drifted — restart with lens. (Literal-string searches and full file reads remain valid for their non-code-symbol use cases.)
- [ ] If P1: relevant code-map notes loaded **and** verified against current source via `lens follow`/`lens refs`/`lens query`? Lens is required — if it was missing the loop should have aborted at bootstrap, not reached here.
- [ ] If P5: code-map updated under `.claude/state/code-map/` for every area touched, with file:line anchors? `lens . --update` run (mandatory — lens is required)?
- [ ] If P5: `README.md` updated with current endpoints/architecture/project state?
- [ ] If P5 and database project: `schema.txt` updated for any schema changes this section?
- [ ] If P6: snapshot written to disk (including "Code-map updates this section") before announcing boundary?
- [ ] Any direct write to `CLAUDE.md` attempted? If yes — **stop, reroute to proposals queue.**
- [ ] If P4 (about to implement a non-trivial task): was a visible `**Task brief (T<n>):**` block posted in the conversation before any code was written, with every required bullet present?
- [ ] If P4.5 (about to spawn super-qa for a task): was a visible `**Super-qa briefing (T<n>):**` block emitted in the conversation before the Agent call?
- [ ] If P5 (about to spawn section-level super-qa): was a visible `**Super-qa briefing (section <n>):**` block emitted in the conversation before the Agent call?
- [ ] If a super-qa verdict came back: did the subagent's reply include a visible `**Super-qa test plan:**` (or `(section)`) block above the structured verdict? If absent — reject the verdict, re-spawn requesting the test plan.
- [ ] If implementing: tests written **and** the suite was run?
- [ ] If a task was just completed: super-qa spawned and `VERDICT: PASS` (zero BLOCKER, zero MAJOR) received? If not — do not mark task done.
- [ ] If a task just achieved QA PASS: were new/changed functions, structs, and non-trivial blocks commented with why-comments before marking complete? If not — add them now.
- [ ] If P4 task complete: changed files archived to `.history/<date>/`?
- [ ] If P4 task complete and database project and this task added/removed/renamed/re-typed any table, column, index, or constraint: was `schema.txt` appended **in the same task**, before the `.history/` snapshot?
- [ ] If P5: section-level super-qa pass spawned and PASS received before announcing audit complete?
- [ ] Any `unwrap()` / `expect()` / `panic!()` introduced? If yes — fix or justify inline.
- [ ] Trailing recap of what you just did? If yes — delete before sending. *(Exception: the three mandated summaries — plan presentation, task close, section close — must use the "Output for the user" format with a `What changed` table, plain English, no protocol jargon, no `file:line` citations, no `BLOCKER`/`MAJOR`/`MINOR`/`code-map`/`P1`–`P6` words inside the user-facing block.)*
- [ ] If emitting a user-facing summary: under word cap (≤200 task close, ≤400 section close, ≤250 plan)? Files-changed table present (for task/section close)? Forbidden words absent?
- [ ] Any claim about a file/function/flag from memory, code-map, or prior context? If yes — verified by reading or grepping it now?
- [ ] 5+ tasks completed in this section? If yes — current response should be P6, not the next task.
- [ ] Did this response `Read` a whole file that lens had already sliced, or `Grep` the tree for a symbol/keyword? If yes — that is a token-discipline violation; use `lens slice`/`lens search` and note the slip.
- [ ] Did this response open an image / PDF / document without running `lens describe` first, or view one without storing a description afterwards? If yes — store it now with `lens describe <file> --text`.
- [ ] If P6: was `lens meter --diff` run and its numbers placed in the user-facing **Tokens** line?
- [ ] If one-shot build mode: does `current-tasks.md` carry the `## Objective` block with verifiable acceptance checks? Is the next section being entered without waiting for a `/clear`, re-anchored from the snapshot + code-map + `lens map`? If declaring done: is every acceptance check ticked with evidence and did the end-to-end verification task PASS?

If any box is unchecked and the action is required by the active phase, do not send the response — finish the missing step first.

---

## Mid-task re-anchor

If you have made **5+ consecutive tool calls without re-stating the active phase or the current task**, stop and re-anchor: declare the phase, restate the task being executed, then continue. Long tool-call chains are where protocol drift starts.

---
