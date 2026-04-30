# Section Snapshot — 2026-04-30 (claude-skill section 4)

## Just completed
- Project: claude-skill (`/Users/sudeepdasgupta/projects/claude-skill`).
- Section: 4 — rename `/super-coder` → `/pro-coder` + close loose ends.
- Tasks closed: T1 ... T8.
- Closure summary:
  - **T1** Renamed `super-coder/` → `pro-coder/` via `git mv`. Updated `pro-coder/SKILL.md` frontmatter (`name: pro-coder`, trigger `/pro-coder`) and 5 internal narrative mentions of the role name in P4.5/P5 copy.
  - **T2** Updated all script references in `scripts/install.sh`, `install-lens.sh`, `uninstall.sh`. Defence-in-depth tail-checks (`*/super-coder` → `*/pro-coder`) and staging-dir prefix (`.super-coder.staging.*` → `.pro-coder.staging.*`) coordinated.
  - **T3** Updated `scripts/test/round_trip.sh` (23 mentions) and `scripts/test/skill_meta.sh` (6 mentions, plus test-name `frontmatter_name_is_super_coder` → `frontmatter_name_is_pro_coder`).
  - **T4** Updated `README.md` (32 mentions). Fixed diagram alignment in the "How it works" box (lines 528-562) — `pro-coder` is 2 chars shorter than `super-coder`, padding adjusted. Tightened `lens init` description at `README.md:446` to match `cli.rs:33` help text wording (closing nit #11).
  - **T5** Updated `CHANGELOG.md`, `CONTRIBUTING.md`, `examples/README.md`. Added a BREAKING CHANGE entry to CHANGELOG describing the rename + manual remediation step (`rm -rf ~/.claude/skills/super-coder` before re-running `install.sh`).
  - **T6** Updated `.claude/state/code-map/scripts-uninstall.md` (4 mentions in scope/anchors/gotchas).
  - **T7** Modified `scripts/uninstall.sh:127-153` to track an `orphans_reaped` counter. When nothing exists at `${dest}` AND orphans were reaped, emits `"reaped N orphan staging dir(s); nothing else at ${dest}"` instead of the misleading `"nothing to remove ... (Already uninstalled.)"`. Closes nit #10.
  - **T8** End-to-end verification: `bash scripts/test/round_trip.sh` → 73 PASS / 0 FAIL. `bash scripts/test/skill_meta.sh` → 27 PASS / 0 FAIL. Total 100/100. Section-level super-qa: PASS, zero BLOCKER, zero MAJOR. Two informational drift items on the code-map note (stale line numbers + the now-fixed gotcha clause) — both reconciled in T6's rewrite.
- QA strategy note: per-task super-qa was deferred to section-level because intermediate states (e.g., directory renamed but scripts still expect old name) fail tests by construction. The composed end state is the verifiable artifact for a coordinated mechanical rename. This is a deliberate exception, not a skip — section-level QA covered every task's diff.
- Test totals: 100 (was 49 → 51 net new across post-section-3 commits; this section added zero new tests, only updated assertions on the renamed paths).

## Code-map updates this section
- `scripts-uninstall.md`: revised. Bumped "Last verified" to 2026-04-30 / section 4. Re-anchored line numbers throughout (script grew with the orphans_reaped counter). Replaced the "reads slightly oddly" gotcha with the new reap-aware-log differentiation gotcha (`uninstall.sh:147-153`). All facts re-verified against current source.

## Verified facts carried forward
- Slash command name + skill directory: `pro-coder` (`pro-coder/SKILL.md:2`). `name: pro-coder` in frontmatter is what `skill_meta.sh:70` asserts on.
- Install destination: `${dest_root}/pro-coder` (`scripts/install.sh:122` src + `:145` dest). Default `dest_root=${HOME}/.claude/skills`.
- Defence-in-depth tail check pattern: `*/pro-coder` (`scripts/uninstall.sh:112`). Refuses any `dest` whose final segment isn't `/pro-coder`.
- Staging-dir prefix: `.pro-coder.staging.*` (created `scripts/install.sh:211`, reaped `scripts/uninstall.sh:142`). Leading dot + `.staging.` infix prevents collision with the main `pro-coder` dir.
- Reap-aware log message (`scripts/uninstall.sh:147-153`): when `${dest}` is empty and `orphans_reaped > 0`, emits `"reaped N orphan staging dir(s); nothing else at ${dest}."` Otherwise emits `"nothing to remove at ${dest}. (Already uninstalled.)"`.
- README diagram (`README.md:528-561`) renders correctly with the renamed label; padding adjusted from `super-coder` to `pro-coder              ` (2 extra trailing spaces) to maintain box-inner-width of 29 columns.
- The internal QA role `super-qa` is intentionally **not** renamed. It's the read-only adversarial reviewer subagent; user did not request the rename.
- Vendored `lens/README.md` and `lens/crates/lens-core/src/meter.rs:11` retain `super-coder` references — vendored upstream content pinned at SHA `a29f523` per `lens/VENDOR.txt`. Not in scope for this section.
- Test suite: 73 (round_trip) + 27 (skill_meta) = 100 passing tests. CI gates on both via `.github/workflows/ci.yml`.

## Open invariants for next section
All Section 1 + Section 2 + Section 3 invariants still hold (see prior snapshots if needed). New invariants from this section:

- The skill name is `pro-coder`. Any future change to this name MUST update at minimum: (a) `pro-coder/SKILL.md` frontmatter `name:` field and trigger reference in `description:`, (b) the `frontmatter_name_is_pro_coder` test in `scripts/test/skill_meta.sh:71`, (c) install src/dest path in `scripts/install.sh:122,145`, (d) defence-in-depth tail check in `scripts/uninstall.sh:112`, (e) staging-dir prefix in `scripts/install.sh:211` AND `scripts/uninstall.sh:131,142`, (f) all README mentions, (g) CHANGELOG entry documenting the rename. The test suite catches (a)+(c)+(e); the others are doc/safety and not test-covered.
- `super-qa` (the QA role) is a separate identifier from `pro-coder` (the main loop / skill name). Find-and-replace operations on either MUST be scoped — they are not interchangeable.
- The orphan-reap counter pattern in `uninstall.sh:127-153` is the canonical way to differentiate "did nothing" from "did partial work" log messages. If similar work is added to install.sh or other scripts, mirror this pattern (counter + branched log).

## Next section
- **Goal:** depends on user direction. Project is in good shape — Tier B + Tier C + loose ends largely closed. Possible next moves:
  1. Commit the section-4 diff (rename, uninstall message, README nit).
  2. Bump release tag / cut a `v0.2.0`-style boundary now that the rename is breaking.
  3. Update remaining cosmetic README nits (#12 — the `--version 0.1.0` claim could be made more verifiable, e.g. by linking to `lens/Cargo.toml:9`).
  4. Address any new ask the user surfaces.
- **Entry blast radius:** depends on chosen task.
- **Open questions for next session:**
  - Should the breaking rename be tagged as a release boundary? Pre-existing users of `~/.claude/skills/super-coder/` need the manual `rm -rf` step from CHANGELOG; surfacing that as a release note (not just CHANGELOG entry) would help.
  - The vendored `lens/README.md` references `super-coder` — eventually upstream lens should update too, but that's a separate repo.

## Resume protocol reminder
- For claude-skill resume: read `README.md`, `pro-coder/SKILL.md`, the four `scripts/*.sh` files plus `scripts/_lib.sh`, both test files, this snapshot, and the three notes under `.claude/state/code-map/`. Re-verify every claim against current source before acting.
- Lens mode: project mode = lens. `.lens/index.db` exists with 1135 symbols indexed. Run `lens . --update` after touching any `.rs`/`.py`/`.ts`/`.js`/`.go` file before P5.

## CLAUDE.md proposals queued
- 0 new proposals appended this section. The rename is repo-mechanical, not a project-wide invariant worth codifying. The naming-discipline invariant from "Open invariants" above lives in the code-map (`scripts-uninstall.md`) and this snapshot, not in CLAUDE.md territory.
