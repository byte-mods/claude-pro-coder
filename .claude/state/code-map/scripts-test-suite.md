# code-map: scripts test suite

**Scope:** `scripts/test/round_trip.sh`
**Last verified:** 2026-04-29 — section 2

## Purpose
Self-contained shell-only regression test suite for the install/uninstall
round-trip and the shared safety helpers. No external test framework; bash 3.2+
plus the scripts under test. Exits 0 = all pass; non-zero = numeric failure
count printed. Designed to run identically on macOS and Linux, in CI or locally.

## Public API
Script — invoked directly. No exported symbols. Five sub-tests:
- `test_canonicalize` (`scripts/test/round_trip.sh:104-122`) — 13 cases on `sc_canonicalize_dest`.
- `test_safe_dest_guard` (`scripts/test/round_trip.sh:124-135`) — 8 cases on `sc_assert_safe_dest`.
- `test_orphan_reap` (`scripts/test/round_trip.sh:137-180`) — 7 cases including dry-run and relative-dest variants.
- `test_round_trip_copy` / `test_round_trip_symlink` (`scripts/test/round_trip.sh:184-263`) — 11 assertions per mode covering install/assert/uninstall/assert plus install + uninstall idempotency.

Total: 49 assertions.

## Invariants
- `set -euo pipefail` (`scripts/test/round_trip.sh:24`).
- Single shared parent jail at `${PARENT_JAIL}` (`scripts/test/round_trip.sh:44`); every sub-test creates child dirs under it via `make_jail` (`scripts/test/round_trip.sh:55-58`). Cleanup is one `rm -rf "${PARENT_JAIL}"` in an EXIT trap (`scripts/test/round_trip.sh:46-51`). This sidesteps the bash subshell-array bug that would arise if `make_jail` tracked jails in a top-level array (`jail="$(make_jail)"` runs `make_jail` in a subshell whose array mutations don't propagate to the parent). Verified zero leak across nominal, off-cwd, back-to-back, and sabotage runs.
- Tests use `/tmp` explicitly, NOT `${TMPDIR}` (`scripts/test/round_trip.sh:44`). macOS `${TMPDIR}=/var/folders/...` would be rejected by the `/var/*` rule in `sc_assert_safe_dest`. On macOS `/tmp -> /private/tmp` symlink, but canonicalisation is textual so the `/tmp/...` form is accepted.
- Round-trip variants pass `--no-lens --no-mcp` to install (no cargo build, no `claude.json` mutation in tests) and `--keep-lens --keep-mcp` to uninstall (lens/mcp were never installed; this avoids needing fake binaries to satisfy cleanup paths).
- `assert_true`/`assert_false` (`scripts/test/round_trip.sh:69-86`) shift the name out of `$@` BEFORE invoking the predicate. A prior version omitted the shift and produced false-positive PASS verdicts on `command not found`; the harness now correctly distinguishes "command failed" from "command name was non-existent".
- Path resolution uses `BASH_SOURCE`-derived `script_dir`/`scripts_dir`/`repo_root` (`scripts/test/round_trip.sh:26-28`) — the suite passes from any cwd including `/`.

## Concurrency model
N/A — single-threaded test driver. Sub-tests are sequential.

## Error idioms
- Each assertion increments `total`; failures increment `failures` (`scripts/test/round_trip.sh:60-67`).
- Final `[[ "${failures}" == 0 ]]` (`scripts/test/round_trip.sh:280`) determines exit status.
- Sub-tests do NOT abort the suite on a failure; they continue and the overall counter reports cumulative failures. This matches CI expectations.

## Callers / callees
- Invoked directly. Mentioned in `README.md:148-152` (Verify section).
- Sources `scripts/_lib.sh` directly to test the helpers without subprocess overhead.
- Spawns `scripts/install.sh` and `scripts/uninstall.sh` as subprocesses for round-trip and orphan-reap tests.

## Gotchas
- The relative-dest sub-test (`scripts/test/round_trip.sh:167-178`) `cd`s into `${jail3}` then invokes `uninstall.sh --dest skills`. It saves and restores `${PWD}` so subsequent sub-tests aren't affected.
- `HOME=/Users/qa-test` is set inside `test_safe_dest_guard` (`scripts/test/round_trip.sh:127`). It does NOT leak to the parent shell because the assertion functions invoke `sc_assert_safe_dest` directly — but the variable IS visible to subsequent sub-tests in the same script. Sub-tests that depend on real HOME run BEFORE `test_safe_dest_guard` or use absolute paths.
- The expected output line in `README.md:150` is `Total: 49, Failures: 0`. If a future contributor adds a sub-test, the README count must be bumped or the expected-output guidance becomes misleading.

## Open questions
- (none)
