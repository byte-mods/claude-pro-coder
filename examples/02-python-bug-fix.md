# Example 2 — Race condition in an async pipeline

## Prompt

> The ingest pipeline occasionally drops events under load. Looks like a
> race in the batcher. Repro is intermittent — about 1 in 10k events when
> we're above 5k events/sec. Fix it and add a test that reliably triggers
> it.

## What to expect

**`P1 — Comprehend`**: the skill restates the symptom (1-in-10k drop, only
under load) and explicitly distinguishes "find the race" from "fix the
symptom by retrying". It pulls the batcher's source via
`lens follow Batcher --budget 1500` and `lens refs flush_batch --limit 20`
to map every call site.

**`P2 — Research`**: walks the call graph. Finds the race candidate (e.g.
a TOCTOU between `len(buffer) >= threshold` and `buffer.clear()` happening
in two coroutines without an `asyncio.Lock`). Cites the exact `file:line`.

**`P3 — Plan`**: short plan, fits one section.

- T1: write a test that **reliably** reproduces the race (deterministic
  scheduling via `asyncio.gather` + `asyncio.Event` to interleave the
  two coroutines).
- T2: fix the race — minimum-surface change, preserves existing API.
- T3: regression test that holds the fix.

Because this fits one section and touches < 5 files, the autonomy gate
**does not trigger** — the skill proceeds to P4 without waiting for ack.

**`P4 + P4.5`**: implements T1 (the failing test), confirms it fails on
unfixed code, then T2 (the fix), then T3. Super-qa runs after each. On
T2, super-qa is likely to push back on the first attempt: "the lock fixes
the buffer race but introduces a new issue — `flush_batch` now blocks
the event loop while the lock is held; consider a `Lock` + `to_thread`
or restructure to remove the shared mutable state." Skill iterates.

**`P5 — Audit`**: requirement matrix shows "race fixed" → covered by T3.
Code-map note for `batcher.py` gets updated with the new invariant
("`flush_batch` and `add_event` MUST NOT race; serialised via `_lock`").
User-facing summary in the clean format.

## What the skill is doing internally

- **Why a failing test first:** "intermittent" bug reports are usually
  unfalsifiable. The skill demands a deterministic repro before claiming
  a fix. If the test cannot be written reliably, the bug is misspecified
  and that's escalated to the user before any code changes.
- **Why super-qa pushes back even after the test passes:** super-qa
  doesn't just check that the test passes. It checks for *new* defects
  introduced by the fix — performance regressions, deadlock risks,
  thread-safety changes that weren't part of the requirement.
- **Why the code-map note is updated, not rewritten:** the existing
  note for `batcher.py` is amended with the new invariant. The history
  of what was understood at each section is preserved through git.
