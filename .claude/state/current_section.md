# Section Snapshot — 2026-05-17 (claude-skill section 5 — v6 protocol upgrade)

## Just completed
- Project: claude-skill (`/Users/sudeepdasgupta/projects/claude-skill`).
- Section: 5 — promote pro-coder to v6 protocol: state files mandatory at every P1, lens required (no fallback), visible chain-of-thought enforced at every task and every super-qa spawn.
- Tasks closed: T1 ... T6.
- Closure summary:
  - **T1** SKILL.md — strengthened Bootstrap Step 1 to re-verify `.history/` and `current-tasks.md` exist at every P1 (not just first invocation). Both have explicit ABORT error strings if creation fails. P1 step 1 rewritten to make "Run bootstrap" mandatory-at-every-P1, not "skip if already done". Hard rules 14 + 15 + Pre-response checklist updated to require the re-verify.
  - **T2** SKILL.md — lens required, all fallback-mode language stripped. Bootstrap Step 5 now ABORTs with an install pointer if `lens` not on `$PATH` (`SKILL.md:128-135`). P1 tooling table dropped the "Fallback mode" column entirely (`SKILL.md:162-176`). P4.5 spawn template (`SKILL.md:301`), P5 spawn template (`SKILL.md:482`), P5 `lens . --update` clause (`SKILL.md:451`), Resume protocol step 5 (`SKILL.md:573`), Hard rule #1 (`SKILL.md:606`), Drift anchor #1 (`SKILL.md:785`), and two checklist lines all rewritten to remove "lens mode" / "fallback mode" framing. Unsupported-language case (lens installed, 0 symbols indexed) explicitly NOT called fallback.
  - **T3** SKILL.md — chain-of-thought enforcement. Three new visible blocks introduced: P4 step 0a `**Chain-of-thought (T<n>):**` with mandatory bullets (goal/why/files+symbols/edge cases/failure modes/verification/out-of-scope) at `SKILL.md:221-237`. P4.5 `**Super-qa briefing (T<n>):**` block before every Agent spawn at `SKILL.md:261-275`. P4.5 spawn template grew a "Step 0 — Chain-of-thought before verdict" instruction (`SKILL.md:288-305`). P5 step 4 grew a parallel `**Super-qa briefing (section <n>):**` block at `SKILL.md:393-410`. P5 section-level spawn template grew the same Step 0 CoT requirement (`SKILL.md:461-478`). New Hard rule #18, new Drift anchor #9, four new Pre-response checklist lines.
  - **T4** README.md + scripts/install.sh — Auto-fallback feature row replaced with "Lens-required" and "Chain-of-thought enforcement" rows (`README.md:69-70`). Prereq table moved Rust from Recommended to Always-required (`README.md:84`). "If cargo is missing" callout rewritten to say the skill refuses to run (`README.md:87`). `--no-lens` flag warning added (`README.md:126`). Three FAQ entries rewritten (`README.md:775`, `:798`, `:801`). Two P1 / protocol-overview cells in the table updated. `scripts/install.sh:245-249` comment rewritten in round-2 QA fix (was "still usable in fallback mode (Read/Grep/Glob)", now describes v6's refuse-to-run policy + later-install-Rust path). `install.sh:267` and `:272` log messages updated.
  - **T5** VERSION bumped 0.2.6 → 0.3.0. CHANGELOG.md `## [0.3.0] - 2026-05-17` entry written with `### BREAKING` (lens required), `### Added` (mandatory visible CoT + super-qa briefings + super-qa CoT), and `### Changed` (every-P1 state-file re-verify, hard rules 17 → 18, drift anchors 8 → 9, README/install.sh/FAQ rewrites).
  - **T6** End-to-end verification: `round_trip.sh` → 73 PASS / 0 FAIL. `skill_meta.sh` → 27 PASS / 0 FAIL. Total 100/100. Initial run of `skill_meta` flagged `no_placeholder_markers` on two pre-existing "TODO marker" strings in SKILL.md — reworded to "annotation comment" in both places (the new tooling-table row and the existing lens-first paragraph). Section-level super-qa round 1: VERDICT FAIL — caught a missed fallback-mode comment at `scripts/install.sh:247`. Fixed; round 2: VERDICT PASS with two carried MINORs (CHANGELOG link footers pre-existing drift; no automated fallback-language regression guard).
- QA strategy note: per-task super-qa was deferred to section-level because T1–T5 are tightly-coupled doc edits to the same protocol contract — intermediate states are incoherent protocols, not verifiable artifacts. Section-level super-qa over the composed v6 diff is the right gate. Same deliberate-exception pattern used in section 4.
- Test totals: 100 (unchanged from prior — doc-only section added no tests).

## Code-map updates this section
- No new code-map notes written. This section was protocol-level work — `pro-coder/SKILL.md` is itself the project's authoritative protocol documentation, and CHANGELOG.md is the versioned history. Re-summarising either inside `.claude/state/code-map/` would duplicate authoritative artifacts. The existing four notes (scripts-uninstall.md, scripts-safety-helpers.md, scripts-test-suite.md, lens-csharp-extractor.md) describe areas not touched by this doc-only section.

## Verified facts carried forward
- v6 of pro-coder protocol is live as of `pro-coder/SKILL.md` HEAD on 2026-05-17. v6 contract: lens required (`SKILL.md:128`), mandatory state-file re-verify at every P1 (`SKILL.md:21-47, :152`), visible CoT at P4 step 0a + super-qa briefings + super-qa internal CoT (`SKILL.md:221, :261, :288, :393, :461`).
- Lens-missing ABORT error string for the agent is at `pro-coder/SKILL.md:131-135`, verbatim copy reproduced in CHANGELOG 0.3.0 entry rationale.
- README.md asserts the v6 contract at three independent locations: feature table (`README.md:70`), prereq callout (`README.md:87`), and FAQ (`README.md:775`). Drift between these and SKILL.md would be a contract failure.
- `scripts/install.sh:245-249` comment is now the canonical place describing what happens when cargo is missing under v6. Any future edit must preserve the "skill will refuse to run, install Rust later, re-run install.sh" semantic.
- Hard rule count: 17 → 18 (`SKILL.md:597-617`). Drift anchor count: 8 → 9 (`SKILL.md:780-789`).
- VERSION file is exactly `0.3.0\n` (6 bytes). CHANGELOG header `## [0.3.0] - 2026-05-17` matches.
- Test suite: 73 (round_trip) + 27 (skill_meta) = 100. CI gates on both. No tests added or removed this section.
- The `no_placeholder_markers` test in `skill_meta.sh` flags whole-word TODO/FIXME/TBD/XXX in SKILL.md unless wrapped in literal `<...>` angle brackets. Prose mentions of these markers (e.g., "a TODO marker is a literal string") trip the test. Use "annotation comment" or another phrasing instead.

## Open invariants for next section
All Section 1 + Section 2 + Section 3 + Section 4 invariants still hold (see prior snapshots). New invariants from this section:

- **The v6 protocol's three visible CoT/illustration blocks are mandatory.** Any future protocol edit MUST preserve: `**Chain-of-thought (T<n>):**` at P4 step 0a, `**Super-qa briefing (T<n>):**` before every P4.5 spawn, `**Super-qa chain-of-thought:**` emitted by super-qa inside its reply. Section-level analogues (`(section <n>)`, `(section)`) are equally mandatory. Internal `<thinking>` is not sufficient — the user must see what the agent and the reviewer are about to do, before they do it.
- **`scripts/install.sh`, `README.md`, and `pro-coder/SKILL.md` are bound by a three-way contract on the lens-required policy.** No single edit may introduce "fallback mode" or "falls back to Read/Grep/Glob" language anywhere in this triad. If future work needs to talk about the retired v5 fallback, do so explicitly as "v5's retired fallback" / "v5 had X; v6 removed it" — always in past tense, always with the v6 contract reaffirmed in the same paragraph.
- **`current-tasks.md` and `.history/` are re-verified at every P1, not just bootstrap.** This is a behavioural change from v5 (where bootstrap was the only check). Future P1 implementations must run the existence check unconditionally.
- **`no_placeholder_markers` test fires on prose mentions of TODO/FIXME/TBD/XXX as whole words.** Use alternative phrasing ("annotation comment", "marker comment", etc.) when documenting the use cases for these patterns. Wrap in `<...>` angle brackets only when documenting them as placeholders.

## Next section
- **Goal:** depends on user direction. The two MINOR defects from QA are worth tracking:
  1. CHANGELOG.md link footers stop at `[0.2.3]` — restore footers for 0.2.4, 0.2.5, 0.2.6, 0.3.0 so the markdown links resolve.
  2. Add an automated regression guard: a `scripts/test/skill_meta.sh` check (or a pre-commit grep) that fails if `pro-coder/SKILL.md`, `README.md`, or `scripts/install.sh` reintroduces "fallback mode" / "falls back to Read" / "falls back to Grep" language.
  3. Or anything new the user surfaces.
- **Entry blast radius:** depends on chosen task. If MINOR #2 is taken, blast radius is `scripts/test/skill_meta.sh` + the three v6-contract files for the test target.
- **Open questions for next session:**
  - Should the install-time lens check be tightened to also fail `install-lens.sh --skip-if-no-cargo` if v6 is the active version? Current behaviour is exit 0 with a warning; v6 puts the actual abort at first invocation. The current split (build-time soft, runtime hard) is defensible but worth a user decision.
  - The `lens . --update` ran at section close and re-extracted 207 symbols / 300 refs from 4 changed + 1 new file — none of those are this section's files (lens doesn't index .md/.sh/.txt). The 5 files are likely from unrelated background changes; worth a `lens . --update --verbose` check if anything looks off, but not blocking.

## Resume protocol reminder
- For claude-skill resume: read `README.md`, `pro-coder/SKILL.md` (the new v6 contract), the four `scripts/*.sh` files plus `scripts/_lib.sh`, both test files, this snapshot, and the four notes under `.claude/state/code-map/`. Re-verify every claim against current source before acting.
- Lens is required (v6 contract). `.lens/index.db` exists. Run `lens . --update` after touching any `.rs`/`.py`/`.ts`/`.js`/`.go`/`.dart`/`.java`/`.cs` file before P5.

## CLAUDE.md proposals queued
- 0 new proposals appended this section. The v6 protocol changes are self-documenting in `pro-coder/SKILL.md` and CHANGELOG.md; they are skill-internal, not project-contract material for `claude-skill`'s own CLAUDE.md.
