# pro-coder reference — modes

_Loaded on demand from `SKILL.md`. Resume protocol (opening a context after P6),
fast path (trivial tasks), and one-shot build mode (end-to-end objectives and
greenfield projects)._

---

## Resume protocol *(opening a new context after P6)*

When the agent starts a session and `.claude/state/current_section.md` exists:

1. Read `CLAUDE.md` (if exists) first — it's the project contract.
2. Read the section snapshot.
3. Treat "Verified facts" as starting hypotheses, not truths — re-verify any that the new section's blast radius touches.
4. Treat "Open invariants" as hard constraints carried forward.
5. **Load the code-map for the new section's blast radius.** Read every relevant note under `.claude/state/code-map/`, then verify against current code with `lens query`/`lens follow`/`lens refs` — lens is required and was verified present at bootstrap. The code-map is a claim about the past; current source is ground truth.
6. Proceed normally from P2.

The snapshot and code-map are **claims about the past**, not the current state of code. Same rule as agent memory: verify before acting.

---

## Fast path *(trivial tasks only)*

For typo fixes, doc updates, single-line renames inside one file, formatting-only changes:

- Skip P3 plan presentation.
- **Still run** P1 code-map load + verification, P4 tests, P5 code-map update (only if the change altered any documented fact — pure typos in comments don't require an update).
- Skip P6 — fast-path tasks don't accumulate enough context to need a reset.
- Announce explicitly at the top: `> fast-path: <reason>`.
- If the change grows beyond trivial mid-implementation, stop and switch to the full loop.

Anything ambiguous is **not** trivial. When in doubt, full loop.

---

## One-shot build mode *(end-to-end objectives and greenfield projects)*

Enter this mode when the user asks for something to be **built / delivered / made to work end-to-end in one go** ("build me X", "make this work", "ship the whole feature"), or when the project has no code yet. The loop is unchanged; what changes is that the agent does not stop at section boundaries and does not declare done until the objective is verified.

1. **Objective contract (P1).** Before P3, write an `## Objective` block at the top of `current-tasks.md`:
   ```markdown
   ## Objective
   - Goal: <one sentence, the user's words>
   - Acceptance checks:
     - [ ] A1: <observable behaviour or runnable command + expected result>
     - [ ] A2: ...
   - Out of scope: <what the user did not ask for>
   ```
   Every acceptance check must be verifiable by a command, a test, or an observable output — "works" is not a check. Ask the user only if the checks cannot be written without a decision they own; otherwise state the assumption in the block and proceed.
2. **Plan every section up front (P3).** Produce the ordered section list (each section ≤ 7 tasks, one architectural goal), and make the **last task of the last section the end-to-end verification** that runs every acceptance check. Present the plan once (autonomy gates still apply); after that, do not re-ask.
3. **Continuous sections (P6 without stopping).** At each section boundary still write the snapshot, update the code-map, run `lens . --update`, and emit the user-facing closure block — then **continue immediately** into the next section in the same context. Re-anchor from disk, not from memory: re-read the snapshot, the relevant code-map notes, and `lens map --budget 1500 [--scope <next area>]` before P2 of the next section. If the context is visibly degrading (recalling instead of looking up, contradicting earlier verified facts), say so in one line, write the snapshot, and ask the user to `/compact` or `/clear` and re-invoke — that is the only stop that is not objective-driven.
4. **Greenfield.** `lens index` reports 0 files at first; that is expected. Create the skeleton in T1 (build file, entry point, test harness), run `lens . --update`, and from then on every task is navigated with lens like any other project. Use `lens deps` at each section entry to confirm the wiring you just built matches the plan.
5. **Done means verified.** The objective is met only when the end-to-end verification task returns `VERDICT: PASS` from super-qa **and** every acceptance check in `current-tasks.md` is ticked with the command/output that proved it. Then, and only then, emit the final closure block and stop. If a check cannot be met, say which one, why, and what was delivered instead — never tick it silently.
6. **Halting conditions that still apply:** stuck-loop detection (same defect twice), dispute abuse, an autonomy-gate decision the objective contract does not already cover (new dependency, public API change, CI change — present once, continue after ack), or a bootstrap abort.

---

