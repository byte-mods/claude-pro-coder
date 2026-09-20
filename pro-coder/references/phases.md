# pro-coder reference — phase mechanics (P1–P6)

_Loaded on demand from `SKILL.md`. The core file carries each phase in summary;
this file is the full mechanics: task briefs, both super-qa spawn templates, the
code-map note format, and the P6 snapshot and proposal formats. Read the section
for the phase you are entering — you do not need the whole file._

---

### P2 — Research

Read every file in the blast radius end-to-end. Cite `file:line` in findings. Catalog: concurrency primitives in use, error idioms, naming conventions, test layout, public API contracts, performance budgets. Enumerate failure modes specific to this change (races, lifetimes, deadlocks, partial writes, dep outages).

### P3 — Plan

Decompose the section into atomic tasks. Each task: ≤100 LOC, one logical concern, explicit dependencies, a named verifying test.

```
Section: <n> — <one-line goal>
Architecture: <textual diagram with concurrency edges>
Spec: crates (with version), data structures (with complexity), concurrency strategy per shared resource, error strategy, integration points
Tasks:
  [ ] T1: <change> — files: <list> — verifies: <test_name>
  [ ] T2: <change> — depends T1 — files: <list> — verifies: <test_name>
  [ ] Tn: end-to-end verification
Verification: unit / integration / property / bench (with target numbers)
Risks: <risk> → <mitigation>
```

**Autonomy default:** proceed without waiting for ack. Wait for explicit user acknowledgment **only** when the plan:

- spans more than one section, **OR**
- touches >5 files, **OR**
- introduces a new dependency, **OR**
- changes a public API or wire format, **OR**
- modifies the build/CI pipeline.

In those cases, **present the plan to the user via the "Output for the user" format** (see that section for hard rules). The internal task list above is for your own tracking — the user sees a clean headline, a "Files I will touch" list, and a "What I will deliver" section. After presenting, stop. Otherwise, advance to P4.

### P4 — Implement & Test *(one task at a time)*

For each task `Ti`:

0. **Update `current-tasks.md` — move `Ti` from `## Queued` to `## In progress` with start timestamp.** Do this before writing any code. This is the ledger that lets a future session pick up where you left off.

0a. **Write the task brief (mandatory, visible).** Before any code is written or any file edited, post a structured brief in the conversation covering the points below. This is a planning artifact authored for the user — they read it to confirm the task has been thought through before any code is touched. It is not optional. Use this exact format:

   ```markdown
   **Task brief (T<n>):**
   - **Goal:** <what this task changes, in one sentence>
   - **Why:** <why this change is needed; reference the P3 plan / user request>
   - **Files + symbols implicated:** <paths and symbols with `file:line` anchors where known>
   - **Edge cases:** <inputs / states / sequences this change must handle correctly>
   - **Failure modes:** <how this could go wrong — races, missing branches, breaking callers, etc.>
   - **Verification approach:** <which tests will exercise this change; what super-qa should probe>
   - **Out of scope:** <what this task deliberately does not change, to prevent scope creep>
   ```

   Every bullet is required. If a bullet genuinely does not apply (e.g. no edge cases for a docs-only change), write `n/a — <one-line reason>` rather than dropping the bullet. Skipping the brief is a protocol violation. **Fast-path exception:** for true typo/format/single-line-rename tasks (see the Fast-path section), the brief collapses to a single `> fast-path: <reason>` line — but if the change touches behaviour, it is not fast-path, and the full brief above is required.

1. Implement. Idiomatic, terse, indistinguishable from surrounding code.
2. Mental compile: lifetimes resolve, trait bounds satisfied, no deadlock from lock ordering, no hot-path allocs, no `unwrap`/`expect`/`panic!` on production paths.
3. Write tests in the **same task**. Naming: `test_<component>_<scenario>_<expected_behavior>`. Cover happy path, edges, errors, concurrency where applicable.
4. Run the full suite. If a pre-existing test breaks, **stop** — do not modify the test. The regression is in the new code.
5. **Hand off to super-qa** *(see P4.5 — mandatory)*. Iterate until super-qa returns `VERDICT: PASS`.
6. **Comment the code.** After QA PASS, add concise why-comments to every function, method, struct, module, and non-trivial logic block written or changed in this task. Explain: invariants upheld, edge cases handled, non-obvious design decisions, and any constraints the code assumes but does not enforce. Use the language's idiomatic doc format (Rust `///`, Python docstrings, JSDoc `/** */`, Go `//`). For dense algorithmic passages, add inline comments explaining the strategy — not what each line does, but why this approach was chosen and what preconditions hold at each step. The audience is a developer (human or AI) reading this code cold six months from now: they should understand the logic without reconstructing your reasoning.
7. **Archive changed files to `.history/`.** For every file modified in this task, copy its final post-task state to `.history/<ISO-date>/<relative-path>` (e.g. `.history/2026-05-03/src/models/user.py`). Preserve relative directory structure. This is a write-only audit trail — never read from `.history/` unless the user explicitly asks.
8. **Update `schema.txt` if database schema changed.** If this task added, removed, renamed, or re-typed any table, column, index, or constraint, append a dated entry to `schema.txt` reflecting the current schema. If `schema.txt` did not exist, create it at project root.
9. **Update `current-tasks.md` — move `Ti` from `## In progress` to `## Completed (this session)` with completion timestamp and a one-line outcome.**
10. Mark `Ti` complete. Advance.

Never carry a half-implemented task forward.

---

### P4.5 — Super-QA loop *(mandatory after every task)*

Every task `Ti` is gated by an independent QA pass. **Spawn a subagent** via the Agent tool (`subagent_type: general-purpose` unless a more specific QA agent is configured) using the prompt template below.

**Isolation guarantee.** Each super-qa spawn runs in a **fresh, isolated context** with zero memory of pro-coder's reasoning, prior conversation, or previous QA rounds. The Agent tool gives this for free — every `Agent(...)` call is a clean slate. This is the equivalent of `/clear` between agents: super-qa only sees what pro-coder explicitly hands it in the prompt. It must rebuild its own understanding from reading code and the project code-map.

**Role boundary (super-qa is read-only).** Super-qa **never** writes, edits, or commits code. Never adds tests. Never proposes patches. Never updates the code-map. Its only output is a structured verdict report. The fix is pro-coder's job — separation prevents super-qa from "helpfully" patching the diff and contaminating the artifact under review. If super-qa wants a test added, it states *which test should exist*; pro-coder writes it next round.

**Write the super-qa briefing (mandatory, visible).** Before invoking the Agent tool, post a visible block in the conversation stating what super-qa is about to verify. This is the same content that goes into the spawn template's context section, but surfaced to the user so they see what is being tested before the subagent runs. Use this exact format:

```markdown
**Super-qa briefing (T<n>):**
- **Task under review:** <Ti name + one-line goal>
- **Requirements to verify:** <verbatim from P3 plan — one bullet per requirement>
- **Files in the diff:** <paths + line ranges>
- **Tests added/changed:** <test names>
- **Performance / correctness budgets:** <e.g. p99 < 5ms, zero panics, idempotent reapply — or "none stated">
- **Adversarial probes super-qa should run:** <specific edge cases, concurrency scenarios, partial-failure inputs to try against this particular diff — not the generic checklist, the *task-specific* probes>
- **Non-obvious gotchas in this diff:** <anything a fresh reviewer might miss without a hint — invariants this change depends on, subtle ordering, hidden coupling — or "none">
```

Every bullet is required. If a bullet does not apply, write `n/a — <one-line reason>` rather than dropping it. Skipping the briefing block is a protocol violation: the user must see what super-qa is testing before it tests, not after.

**Spawn template:**

```
You are super-qa: an adversarial QA reviewer. You did not write this code and you do not trust it. You have NO memory of any prior conversation.

Context handed to you (this is all you know):
- Task being verified: <Ti name + one-line goal>
- Requirements (from P3 plan): <list — verbatim>
- Files changed in this task: <paths + line ranges>
- Tests added in this task: <test names>
- Performance budget (if any): <e.g. p99 < 5ms on hot path>
- Code-map notes relevant to the changed area: <list of files under .claude/state/code-map/ — read them, but treat as claims, not truth>
- Previous failures addressed (if iteration > 1): <numbered list of fixes from prior round>

Your job:

**Step 0 — Test plan before verdict (mandatory, visible).** Before running probes or producing the verdict, post a structured test plan in your reply stating exactly what you are about to test and why. It must appear in your response above the structured verdict. Use this exact format:

```markdown
**Super-qa test plan:**
- **Requirements as I read them:** <verbatim list of the requirements you were handed; if any are ambiguous, name the ambiguity>
- **What the diff actually does, per requirement:** <one bullet per requirement, paraphrased from your code read with `file:line` anchors>
- **Where they could diverge:** <for each requirement, the specific way the diff could fail to satisfy it — missing branch, wrong order, off-by-one, etc.>
- **Edge cases I will probe and why:** <task-specific edges, not the generic checklist — what *this* diff is most likely to break on>
- **What I will run:** <which tests, which adversarial inputs, which code reads — concrete plan>
- **What would change my verdict:** <the smallest piece of evidence that would flip PASS↔FAIL>
```

Every bullet is required. If a bullet does not apply, write `n/a — <one-line reason>`. A verdict posted without its test plan is rejected — pro-coder will re-spawn you and ask for it.

**Step 1.** Read the listed code-map notes for context, then map the actual blast radius yourself. Use `lens follow <symbol>` and `lens refs <symbol>` for budget-capped slices — lens is required by the protocol and is guaranteed to be present. Reach for `Read`/`Grep` only for non-lens jobs (literal strings, full file bodies prior to a final adversarial read, unsupported-language projects). Do not trust the author's framing or the code-map's framing — verify both against current source.

2. Read every changed file end-to-end and the new tests.
3. Run the test suite. Report exit status.
4. Adversarial probe — for each requirement, attempt to construct an input or sequence that breaks it. Specifically check:
   - empty / zero / max-size / malformed inputs
   - concurrent callers (10K) where applicable
   - dependency unreachable / slow / partial-failure
   - config reload mid-flight
   - memory pressure / allocation on hot paths
   - error path coverage (every Result/Option branch tested?)
   - panics: any `unwrap` / `expect` / `panic!` on production paths?
   - blocking calls inside async functions?
   - Mutex on a declared hot path?
5. Compare implementation against requirements. List any requirement not verified by a test.
6. Return a structured verdict using the exact format below. Tag every defect with severity:
   - **BLOCKER** — broken correctness, panic on production path, race, data loss, requirement unmet. Task cannot close.
   - **MAJOR** — defect that does not corrupt data but degrades reliability or perf below stated budget. Task cannot close.
   - **MINOR** — code-quality risk, missing edge-case test, suboptimal but correct. Logged, does not block.

   ```
   VERDICT: <PASS | FAIL>
   Summary: <one line — what was verified, or what blocks>

   Verified requirements:
   - Req <n>: <statement> → covered by <test_name> at <file:line>
   - ...

   Defects:
   - [BLOCKER] <file:line> — <defect> — repro: <input/sequence or missing test name>
   - [MAJOR]   <file:line> — ...
   - [MINOR]   <file:line> — ...
   (omit a tier if empty)

   Code-map drift (if any):
   - <code-map file>: <claim> contradicts <file:line>
   ```

   `VERDICT: PASS` requires zero BLOCKER and zero MAJOR. MINOR may exist on a PASS — they are tracked, not blocking. "Code-map drift" is informational; pro-coder reconciles it at P5.

Do not speculate. Do not suggest stylistic changes. Only report defects grounded in code reads, test runs, or requirement gaps. Never write code. Never edit code-map notes.

Reply in under 500 words.
```

**Loop rules:**

- On `VERDICT: FAIL` — return to P4 step 1 for this task. Address every BLOCKER and MAJOR defect. Re-run tests. Re-spawn super-qa with the same context plus a `Previous failures addressed:` line listing what was fixed (one line per defect, citing file:line). Do not advance until `PASS`.
- On `VERDICT: PASS` — record the one-line verification summary alongside the task in the plan checklist (`[x] T2 — qa: <summary>`). Append any MINOR defects from the PASS to a follow-up task in the plan (don't drop them silently). If super-qa reported code-map drift, log it for reconciliation at P5. Advance to the next task.
- **Loop until super-qa is satisfied.** No fixed iteration cap. Iterate as many rounds as needed.
- **Stuck-loop detection (the only escape hatch).** If super-qa returns *the same defect* (same file:line, same root cause) **twice in a row** after a fix attempt, the loop is stuck — the task spec or the fix approach is wrong, not the implementation effort. Stop, escalate to the user with the recurring defect verbatim, and treat the task as misspecified: return to P3 and re-decompose. Never advance silently.
- **Dispute protocol** *(use sparingly — only when super-qa is provably wrong)*. If pro-coder believes a defect is a false positive (e.g., super-qa claims a test is missing but it exists, or claims a path is unreachable when it is reachable):
  1. Re-spawn super-qa with the same context plus a `Disputed: <defect>` block containing **file:line evidence** that disproves the claim — a test name, a code reference, an output snippet.
  2. Super-qa adjudicates: either issues a corrected verdict (defect withdrawn) or restates the defect with a sharper repro.
  3. If super-qa upholds the defect after evidence, pro-coder must accept and fix — super-qa's verdict is final on a second look.
  4. Disputes do not count toward stuck-loop detection unless the *same disputed defect* recurs across a fix attempt. Abuse of dispute (more than one dispute per task) is a smell — escalate to user.
- Super-qa runs **per task** in P4.5 *and* once at section close in P5 (integration-level review of the cumulative diff).
- Trivial fast-path tasks (typo, doc tweak, single-line rename) skip super-qa. If a "trivial" change touches behaviour, it isn't trivial — run the full P4 + P4.5.

**Why this exists:** the author of code is the worst reviewer of it. An independent context with no exposure to the original reasoning catches the failure modes the author has already rationalised away. The loop is unbounded by design — premature exit hides defects. Severity tiering keeps cosmetic noise from blocking shipments while keeping correctness defects fatal. Stuck-loop detection fires only when the *same* defect recurs, signalling a spec problem. The dispute protocol exists because super-qa can be wrong too, but the bar is high — file:line evidence, not argument.

### P5 — Audit & Code-map update *(section exit)*

1. Build a requirement-traceability matrix:
   ```
   - Req 1: <statement> → <file:line> (verified by <test_name>)
   - Req 2: ...
   ```
   If any requirement is unmet, return to P3.
2. Adversarial review: empty/max/malformed input, 10K concurrent callers, dep unreachable, slow dep (timeout), config reload mid-flight, memory pressure. If a flaw surfaces, return to P3 — do not patch in place.
3. Performance audit: hot-path allocs, unnecessary locks, blocking calls in async, redundant clones.
4. **Section-level super-qa spawn** *(mandatory, integration-level)*. Spawn super-qa once more with the cumulative section diff, not just the last task. Per-task QA proved each task individually; this pass proves they compose. Use the spawn template below. Iterate to PASS using the same loop rules as P4.5.

   **Write the section briefing (mandatory, visible).** Same rule as P4.5: before the Agent call, post a visible block in the conversation stating what super-qa is about to verify at the section level. Use this exact format:

   ```markdown
   **Super-qa briefing (section <n>):**
   - **Section goal:** <one-line>
   - **Tasks composed:** <T1...Tn — names + one-line goals each>
   - **Cumulative files in the diff:** <paths>
   - **Cumulative tests added:** <test names>
   - **Per-task PASS verdicts:** <T1 summary; T2 summary; ...>
   - **Performance / correctness budgets in play:** <list, or "none stated">
   - **Open invariants from prior sections that this section must not have broken:** <list>
   - **Composition-failure probes super-qa should run:** <integration-level edges specific to *this* section's tasks — not the generic checklist>
   - **Non-obvious cross-task gotchas:** <data flowing T1→T3 dependencies, shared state, ordering — or "none">
   ```

   Every bullet required. If a bullet does not apply, write `n/a — <one-line reason>`. Skipping the briefing block is a protocol violation.
5. **Mandatory: update the code-map.** For every module/file/subsystem touched this section, write or revise a note in `.claude/state/code-map/` capturing what you now understand about that area. Reconcile any "Code-map drift" reports from super-qa. The code-map is the project's persistent memory of code structure — what gets written here outlives sections and conversations. Format below.

   **Run `lens . --update`** (incremental — re-extracts only changed files) so the symbol index reflects the section's diff. The `.lens/index.db` is what powers the next P1's `lens query`/`lens follow` calls; stale indexes mean P1 reads stale slices. This is mandatory at every section close — lens is required by the protocol, the index is the artifact that makes it useful.
6. Mark all section tasks `[x]`. Produce a **user-facing closure summary** using the format in the "Output for the user" section — clean headline, `What changed` table, `Why it matters`, `Tests`, and `What's next` if anything is deferred. Internal closure detail (≤5-bullet technical recap) goes into the snapshot at P6, not into the user-facing block.
7. **Update `README.md` with project state.** If `README.md` exists, update it with the current architecture overview, endpoint list (if applicable), and any materially changed project facts surfaced during this section. If it does not exist, create it with a concise project summary. Preserve any human-written narrative sections; only update factual/structural blocks (endpoints, architecture diagrams, setup steps).
8. Update agent memory only for non-obvious architectural patterns, performance constraints, or stakeholder context. **Never save code-derivable facts to agent memory** — those go in the code-map.

**Code-map note format** *(one file per area; filename is `<area-slug>.md`, e.g. `runtime-scheduler.md`, `payments-pipeline.md`, `wire-protocol.md`)*:

```markdown
# code-map: <area>

**Scope:** <files / modules covered>
**Last verified:** <ISO date> — section <n>

## Purpose
<1–3 sentences: what this area does and why it exists>

## Public API
- `<symbol>` (`<file:line>`) — <one-line contract; inputs, outputs, error conditions>

## Invariants
- <invariant> — enforced at `<file:line>`

## Concurrency model
- <shared resource>: <lock-free / sharded / Mutex / channel / actor> (`<file:line>`)
- <hot path declaration if any>

## Error idioms
- <pattern, e.g. "Result<T, DomainError> with thiserror; never panics on production paths"> — `<file:line>`

## Callers / callees
- <upstream caller> → `<symbol>` (`<file:line>`)
- `<symbol>` → <downstream dep> (`<file:line>`)

## Gotchas
- <non-obvious behavior, footgun, or historical reason> (`<file:line>`)

## Open questions
- <unresolved item to revisit; remove when answered>
```

**Code-map hygiene rules:**

- Every fact carries a `file:line` anchor. No anchor → not a fact, drop it.
- Notes describe **what is**, not **what should be**. Aspirations belong in CLAUDE.md proposals.
- If a prior note contradicts what you saw this section, **correct it** and record the diff in the section snapshot's "Verified facts carried forward".
- Never copy-paste large code blocks into notes. Notes summarise; code is the source of truth.
- One file per area. If two areas merge, merge the notes. If one area splits, split the notes.
- Maximum ~200 lines per note. Past that, the area is too broad — split it.

State explicitly: **"Audit complete. Code-map updated."** If you cannot say this honestly, do not say it.

**Section-level super-qa spawn template:**

```
You are super-qa: an adversarial integration reviewer for a completed section. You did not write this code. You have NO memory of any prior conversation.

Context handed to you (this is all you know):
- Section goal: <one-line>
- Tasks completed in this section: <T1...Tn — names + one-line goals>
- Cumulative files changed across the section: <paths>
- Cumulative tests added: <test names>
- Per-task QA verdicts: <T1: PASS — <summary>; T2: PASS — <summary>; ...>
- Performance budgets (if any): <list>
- Open invariants from prior sections (if any): <from .claude/state/current_section.md>
- Code-map notes relevant to changed areas: <list of files under .claude/state/code-map/>

Your job — integration-level review:

**Step 0 — Test plan before verdict (mandatory, visible).** Before running anything or producing the verdict, post a structured test plan in your reply stating what you are about to test at the integration level. It must appear above the structured verdict. Use this exact format:

```markdown
**Super-qa test plan (section):**
- **Section goal as I read it:** <verbatim from briefing; name ambiguity if any>
- **How the tasks compose, per the cumulative diff:** <T1→T2→...→Tn data/control flow, paraphrased from your reading with `file:line` anchors at each hand-off>
- **Composition failure modes I will probe:** <integration-level edges no single-task review could catch — e.g., T1 allocates and T4 calls in a loop on a hot path; T2 changes the error shape T5 pattern-matches on; two tasks add overlapping validation>
- **Open invariants from prior sections, and how I will check each still holds:** <one bullet per carried-forward invariant>
- **What I will run:** <which tests at integration level, which adversarial multi-task sequences, which code-map cross-references>
- **What would change my verdict:** <the smallest piece of integration-level evidence that would flip PASS↔FAIL>
```

Every bullet is required. If a bullet does not apply, write `n/a — <one-line reason>`. A verdict posted without its test plan is rejected.

**Step 1.** Read the listed code-map notes, then trace how the section's pieces connect to the rest of the codebase. Use `lens follow`/`lens refs`/`lens path "A" "B"` for symbol-aware slices — lens is required by the protocol and is guaranteed to be present. Reach for `Read`/`Grep` only for non-lens jobs (literal strings, full file bodies, unsupported-language projects). Verify the code-map against current source — do not trust either blindly.
2. Read the cumulative diff end-to-end as a single unit. Check things that no individual task review could catch:
   - Tasks pass individually but break when composed (data flowing T1→T3 violates an invariant).
   - Two tasks add overlapping responsibilities (duplicate validation, conflicting locks).
   - Public API surface added across tasks is inconsistent (naming, error shapes, async-ness).
   - Integration tests covering the cumulative path exist? If not, name what's missing.
   - Cross-task perf interactions (T1 allocates, T4 calls it in a loop on a hot path).
   - Open invariants from prior sections still hold?
3. Run the full test suite (not just the new tests). Report exit status.
4. Return the same structured verdict format as P4.5 (PASS/FAIL with BLOCKER/MAJOR/MINOR severity tags + Code-map drift section).

Do not re-litigate per-task defects already closed. Focus on integration. Never write code. Never edit code-map notes.

Reply in under 500 words.
```

### P6 — Section Boundary *(context reset + CLAUDE.md proposals)*

After P5 closes a section, **before** starting the next section, perform a hard context reset.

**Triggers — any one of these forces a P6 reset:**

- Section's tasks all complete (`[x]`) and there is more work queued.
- 5+ tasks completed in the current section.
- A subjective sense that earlier reasoning is being referenced more than re-verified — i.e., the agent finds itself recalling instead of looking up.
- The user invokes the keyword `section boundary` or `reset`.

**Reset protocol:**

1. **Snapshot to disk.** Write `.claude/state/current_section.md` with this exact structure:
   ```markdown
   # Section Snapshot — <ISO timestamp>

   ## Just completed
   - Section: <n>
   - Tasks closed: T1 ... Tn
   - Closure summary: <P5 bullets verbatim>

   ## Code-map updates this section
   - <area-slug>.md: <created | revised — one-line summary of what changed>

   ## Verified facts carried forward
   - <fact + file:line evidence> (one per line, only facts grounded in code reads from this section)

   ## Open invariants for next section
   - <constraint discovered this section that affects upcoming work>

   ## Next section
   - Goal: <one-line>
   - Entry blast radius: <files/symbols to load code-map for on resume>
   - Open questions: <if any>
   ```
   No prose narration. Bullets only. This file is read by the next session — write for that audience.

2. **Persist to agent memory** anything from "Verified facts carried forward" or "Open invariants" that will outlive this project context (compliance, stakeholder, architectural constraints). Do not duplicate code-derivable facts — those live in the code-map.

3. **CLAUDE.md proposals (append-only).** If during this section you observed a project convention, anti-pattern, or constraint that *should* be in CLAUDE.md but isn't, append a proposal to `.claude/state/claude_md_proposals.md`. Format:
   ```markdown
   ## Proposal — <ISO timestamp> — Section <n>

   **Suggested addition:**
   <verbatim text to add to CLAUDE.md, ready to copy-paste>

   **Section:** <which CLAUDE.md heading it belongs under, e.g. `## Concurrency`>

   **Justification:**
   <one-paragraph: what was observed, what files/lines support it, why it deserves to be a project-wide rule>

   **Confidence:** <high | medium | low>

   ---
   ```
   **Hard rules for proposals:**
   - Never write to `CLAUDE.md` directly. Only to `claude_md_proposals.md`.
   - Only propose things grounded in *this section's* code reads with file:line evidence.
   - Never propose stylistic preferences, only invariants the code actually demands.
   - Maximum 3 proposals per section. If you have more, you're over-fitting to local observation — pick the strongest 3.
   - If `claude_md_proposals.md` does not exist, create it with header `# CLAUDE.md Proposal Queue\n\n_Pending review by user. Accepted entries to be copied into CLAUDE.md by hand._\n\n`.

4. **Announce the boundary** to the user, in the user-facing summary format. The internal artifacts (snapshot, code-map updates, proposals) have already been written; the user sees a clean closure block:

   ```markdown
   ## <plain-English headline — what the section delivered>

   **What changed**

   | File | Change |
   |---|---|
   | `<path>` | <one short sentence in plain English> |
   | ...      | ... |

   **Why it matters**

   <1–3 sentences in plain English explaining what's different from the user's perspective.>

   **Tests**

   <one-line status — e.g. "61 tests pass (was 49 → 12 new added)." Or "No tests run — docs-only section.">

   **Tokens**

   <one line from `lens meter --diff` — e.g. "lens served ~7k tokens in place of ~170k (24× leverage).">

   **What's next**

   - <one or two lines on what's deferred or recommended>
   - Suggest `/clear` before the next section so the context window starts fresh *(omit in one-shot build mode — the next section starts immediately)*.
   ```

   **Do not** mention `P6`, `snapshot`, `code-map`, `CLAUDE.md proposals`, or any other protocol jargon inside this block. Those facts are recorded in the snapshot file already; the user does not need to see the audit trail in their conversation.

5. **Stop** — unless in *One-shot build mode*. Outside that mode, do not begin the next section in the same context: the user runs `/clear` (or `/compact` if they want to preserve some history) and re-invokes with the next section's prompt. In one-shot build mode, re-anchor from disk (snapshot + code-map + `lens map`) and continue into the next section immediately.

**Why this exists:** session context accumulates wrong assumptions. After 5+ tasks, even verified facts get confused with hallucinated ones. A clean window + a structured snapshot + a persistent code-map is more reliable than a long context with everything in it. Reloading the relevant code-map notes on resume costs seconds and prevents the entire class of "Claude remembered something that isn't true" failures.

---

