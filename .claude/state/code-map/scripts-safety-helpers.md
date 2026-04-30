# code-map: scripts safety helpers (_lib.sh)

**Scope:** `scripts/_lib.sh`
**Last verified:** 2026-04-29 — section 2

## Purpose
Single source of truth for the destination-safety guard shared by
`install.sh`, `uninstall.sh`, and `install-lens.sh`. Centralises the unsafe-path
case-list (formerly duplicated across two scripts) and the canonicalisation
that closes a `..`-traversal bypass.

## Public API
- `sc_set_default_home` (`scripts/_lib.sh:34-37`) — `: "${HOME:=}"`. Idempotent. Required because `set -u` would otherwise trip on an unset HOME before the case-match in `sc_assert_safe_dest`.
- `sc_canonicalize_dest <path>` (`scripts/_lib.sh:39-100`) — emits an absolute, `'.'`-and-`'..'`-resolved path on stdout. Pure bash 3.2; no `realpath`, no `cd`, no `stat`. Path need not exist. Output is purely textual normalisation (does NOT resolve symlinks). Empty input → empty output.
- `sc_assert_safe_dest <path> <prefix>` (`scripts/_lib.sh:103-138`) — canonicalises `<path>` first, then case-matches. Returns 0 if safe, 1 with stderr message if unsafe. `<prefix>` is the script name to prepend to error messages. Error output cites BOTH the raw input and the resolved form so users see what was actually rejected.

## Invariants
- `sc_canonicalize_dest` is **purely textual** — it never touches the filesystem (`scripts/_lib.sh:50-99`). This is a deliberate design choice so the helper works on paths that don't yet exist (e.g., `${dest_root}` before `mkdir -p`).
- `sc_canonicalize_dest` always pops the LAST array index in the `..` branch (`scripts/_lib.sh:88-90`) — never leaves bash array holes. This matters under bash 3.2 + `set -u` where iterating a sparse array with `"${arr[@]}"` can trip "unbound variable".
- The library is sourced, never executed. Sourcing guarded by `SC_LIB_LOADED=1` sentinel (`scripts/_lib.sh:25-28`) — re-sourcing is a no-op.
- The unsafe-path case list (`scripts/_lib.sh:117-127`) covers BOTH macOS-specific paths (`/Applications`, `/Network`, `/Volumes`, `/private`, `/System`, `/Library`) AND Linux-specific paths (`/home`, `/root`, `/srv`, `/run`, `/lib`, `/lib64`, `/mnt`, `/media`, `/boot`, `/proc`, `/sys`, `/dev`, `/etc`, `/var`, `/usr`, `/opt`, `/bin`, `/sbin`). The same script runs cross-platform.

## Concurrency model
N/A — config-time validation, single-threaded shell.

## Error idioms
- Helpers return non-zero with a stderr message keyed by the `<prefix>` arg (`scripts/_lib.sh:111, 114, 132, 135`). Callers `|| exit 1` to surface the failure.
- No `unwrap`-style aborts; every error path is a clean return for the caller to handle.

## Callers / callees
- `scripts/install.sh:28` sources `_lib.sh` via `BASH_SOURCE`-derived path (survives symlinked invocation).
- `scripts/install.sh:30` calls `sc_set_default_home`.
- `scripts/install.sh:93` calls `sc_assert_safe_dest "${dest_root}" "install.sh"`.
- `scripts/uninstall.sh:27, 29, 71, 124` — same pattern (HOME default + safe-dest twice: once for the skills root, once for the lens bin dir).
- `scripts/install-lens.sh:28, 30, 75` — same.
- `scripts/install-mcp.sh:28, 30` — sources for HOME default; does NOT call `sc_assert_safe_dest` (validates via Python instead).
- `scripts/test/round_trip.sh:31` — sources for testing the helpers directly.

## Gotchas
- `${TMPDIR}` on macOS resolves to `/var/folders/...` which IS in the unsafe-dest list (`/var/*`). This is correct production behaviour but means tests cannot use `${TMPDIR}` as `--dest`. Tests use `/tmp` directly (`scripts/test/round_trip.sh:48`). On macOS `/tmp -> /private/tmp` symlink, but canonicalisation is textual so `/tmp/foo` stays `/tmp/foo` and is not rejected by the `/private/*` rule.
- The case-list at `scripts/_lib.sh:117-127` MUST be kept in sync with reality — adding a new system path requires editing here ONCE (was twice before consolidation). The duplicated form previously at `install.sh:73-83` and `uninstall.sh:48-58` is gone.
- `sc_canonicalize_dest` clamps at root: `"/../"` → `"/"`. Consistent with `realpath -m`'s behaviour. Callers that need to reject root-pinning bypasses must combine canonicalisation with a positive allow-list.

## Open questions
- (none)
