# Example 3 — Cross-language refactor

## Prompt

> Auth is duplicated. There's a JWT-validation flow in our Python API
> server (`api/auth.py`) and the same flow in our TypeScript admin
> dashboard (`web/src/lib/auth.ts`). They've drifted — the Python side
> rejects tokens older than 24h, the TypeScript side accepts up to 30
> days. Pick one source of truth, port the other, and add cross-language
> tests that pin the contract.

## What to expect

**`P1 — Comprehend`**: this is a multi-section change touching > 5 files
across two languages, with a public-API decision (which TTL wins?) that
the user owns. The skill flags the autonomy gate **explicitly** at P1:
"this plan will require your acknowledgment before P4 because it touches
a public contract."

The skill calls `lens follow JWT --budget 1500`. Lens detects the symbol
in **both** Python and TypeScript and surfaces both candidates with the
"cross-language: python, typescript" tag. The skill lists both
implementations side-by-side, with the divergence (24h vs 30d) called out
explicitly.

**`P2 — Research`**: pulls the code-owner / commit history for each
implementation if available. Surfaces the question: which TTL is correct?
This is a stakeholder decision, not a technical one — the skill asks the
user once, presenting both options with their security implications.

**`P3 — Plan`** (after the user picks the TTL): multi-section plan.

- **Section 1:** consolidate the canonical implementation in one place.
- **Section 2:** port the other side to consume the canonical impl
  (or replicate it byte-for-byte if the languages cannot share code).
- **Section 3:** cross-language test fixtures that pin the contract.

Skill **presents the plan via the user-facing summary format** (clean
*What I will deliver* list, no protocol jargon) and **stops** for ack.

**Per section, full P4 + P4.5 + P5 + P6.** Each section starts with a
clean context — no two sections share a context window. The P6 boundary
includes a `recommend /clear` line.

## What the skill is doing internally

- **Why the autonomy gate triggers:** the rules are explicit — > 5 files
  OR > 1 section OR public API change → present the plan and wait. This
  hits all three.
- **Why the user picks the TTL, not the skill:** technical correctness
  cannot decide between "24h" and "30d" — that's a security/UX tradeoff
  the user owns. The skill surfaces the decision; it does not make it.
- **Why cross-language tests:** the contract (token format, claim names,
  TTL) is the source of truth, not either implementation. A fixture file
  with known-good and known-bad tokens, consumed by both Python and TS
  test suites, prevents silent drift. Lens helps locate both test suites
  via `lens map --depth 2 --scope api/tests` + `lens map --depth 2 --scope web/src/__tests__`.
- **Why three sections, not one:** > 5 files in one section is past the
  context-window safety threshold. The skill enforces section boundaries
  every 5+ tasks even if you don't ask for them.
