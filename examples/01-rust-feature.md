# Example 1 — New Rust feature, performance-budgeted

## Prompt

> Build a token-bucket rate limiter for our Tokio HTTP client. Constraints:
> shared across worker threads, lock-free on the hot path, must add ≤ 50 ns
> per request when there's headroom. Add property tests.

## What to expect

The skill responds with **`P1 — Comprehend`** at the top, then runs:

1. Bootstrap (silent if already done).
2. Restates the goal: lock-free rate limiter, 50 ns budget on the fast path,
   property tests. Surfaces no clarifying questions because the constraints
   are concrete.
3. Builds the blast radius: any existing limiter, the HTTP client module,
   shared-state primitives in use. Calls `lens query "rate limiter"`,
   `lens follow HttpClient`, `lens refs token_bucket` to pull tight slices
   instead of reading whole files.

Then **`P2 — Research`**: walks the cited code, surfaces the existing
concurrency patterns, names the data structure that fits (atomic counter +
deadline-stamped refill, no Mutex on the hot path).

Then **`P3 — Plan`**: presents a numbered task list, each ≤ 100 LOC with
a named verifying test. Because this introduces a new dep (no existing
crate provides what's needed), the autonomy gate triggers — the skill
**stops and waits for your ack**, presenting the plan in the
"Output for the user" format with a *Files I will touch* list.

After ack, **`P4 — Implement & Test`** for each task one at a time. Tests
ship in the same task. After each task, **`P4.5 — Super-QA loop`** spawns
an adversarial reviewer in a fresh context that did not see the planning
or implementation reasoning. It runs the test suite, traces the diff,
adversarially probes the hot path. Returns `VERDICT: PASS` or `FAIL`
with severity-tagged defects. The skill iterates until PASS.

At section close, **`P5 — Audit`** runs the section-level super-qa
(integration-level, on the cumulative diff), then writes/updates code-map
notes under `.claude/state/code-map/`, then emits a **clean user-facing
summary** with a *What changed* table, *Why it matters*, *Tests*, and
*What's next* — no protocol jargon.

## What the skill is doing internally

- **Why lens not Read:** `lens follow HttpClient --budget 1500` returns
  ~1500 tokens regardless of the file size. `Read` on the same file
  could return 10–50 k tokens. Across the per-task super-qa loops which
  re-traverse the blast radius, the savings compound.
- **Why super-qa runs in a fresh context:** the author of code is the
  worst reviewer of it. A reviewer with no memory of why a decision was
  made catches the failure modes that the author rationalised away.
- **Why a code-map note gets written at P5:** the next session resuming
  this work doesn't need to re-derive the rate limiter's invariants from
  scratch. The code-map note is durable memory, scoped per-area, with
  `file:line` anchors for every claim.
