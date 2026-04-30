#!/usr/bin/env bash
# Register the lens MCP server entry in ~/.claude.json so Claude Code spawns it
# at startup and exposes lens_follow / lens_refs / lens_query / lens_explain /
# lens_path / lens_slice / lens_map as structured tools.
#
# Idempotent: re-running with the same lens-bin path is a no-op. The merge
# touches ONLY the `mcpServers.lens` key; every other key in claude.json is
# preserved byte-for-byte through Python's json round-trip.
#
# Usage:
#   scripts/install-mcp.sh                        # add/update the entry
#   scripts/install-mcp.sh --lens-bin PATH        # custom lens binary path (default: ~/.claude/bin/lens)
#   scripts/install-mcp.sh --claude-json PATH     # custom claude.json (default: ~/.claude.json)
#   scripts/install-mcp.sh --remove               # remove the entry (uninstall)
#   scripts/install-mcp.sh --dry-run              # print the diff without writing
#   scripts/install-mcp.sh --quiet                # suppress non-error output
#   scripts/install-mcp.sh --strict               # extra paranoia — refuse paths outside ~/.claude/
#   scripts/install-mcp.sh --allow-root           # opt in to running as root (default: refuse)
#
# Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE` forms.
#
# Safety:
#   - Backs up claude.json to claude.json.bak.YYYYMMDD-HHMMSS before any write.
#   - Atomic write: stages to a sibling temp file, then mv on the same FS.
#   - If lens binary doesn't exist at --lens-bin, prints a warning and bails
#     (skip rather than register a broken entry that confuses Claude Code).

set -euo pipefail

_sc_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_lib.sh
. "${_sc_script_dir}/_lib.sh"

sc_set_default_home

lens_bin="${HOME}/.claude/bin/lens"
claude_json="${HOME}/.claude.json"
remove=0
dry_run=0
quiet=0
strict=0
allow_root=0

require_value() {
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "install-mcp.sh: $1 requires a value" >&2
    exit 2
  fi
}

require_eq_value() {
  if [[ -z "${2:-}" ]]; then
    echo "install-mcp.sh: $1 requires a value" >&2
    exit 2
  fi
}

log() {
  if [[ "${quiet}" != 1 ]]; then
    echo "$@"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lens-bin)      require_value "--lens-bin" "${2:-}"; lens_bin="$2"; shift 2 ;;
    --lens-bin=*)    require_eq_value "--lens-bin=" "${1#--lens-bin=}"; lens_bin="${1#--lens-bin=}"; shift ;;
    --claude-json)   require_value "--claude-json" "${2:-}"; claude_json="$2"; shift 2 ;;
    --claude-json=*) require_eq_value "--claude-json=" "${1#--claude-json=}"; claude_json="${1#--claude-json=}"; shift ;;
    --remove)      remove=1; shift ;;
    --dry-run)     dry_run=1; shift ;;
    --quiet)       quiet=1;   shift ;;
    --strict)      strict=1;  shift ;;
    --allow-root)  allow_root=1; shift ;;
    -h|--help)
      sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "install-mcp.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

sc_assert_not_root "${EUID}" "${allow_root}" "install-mcp.sh" || exit 1

if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${lens_bin}"    "${HOME}" "install-mcp.sh" || exit 1
  sc_assert_strict_allowed "${claude_json}" "${HOME}" "install-mcp.sh" || exit 1
fi

# Ensure Python 3 is available — required for safe JSON surgery. We do not
# fall back to bash/jq because bash JSON is fragile and jq is not guaranteed
# present on macOS.
if ! command -v python3 >/dev/null 2>&1; then
  echo "install-mcp.sh: python3 not found on PATH. Install Python 3 to enable MCP auto-wire," >&2
  echo "install-mcp.sh: or wire lens manually by adding to ${claude_json}:" >&2
  echo "install-mcp.sh:   \"mcpServers\": { \"lens\": { \"command\": \"${lens_bin}\", \"args\": [\"mcp\"] } }" >&2
  exit 1
fi

# When adding (not removing), ensure the lens binary actually exists. A broken
# command in mcpServers makes Claude Code emit confusing startup errors — far
# worse than a missing entry.
if [[ "${remove}" != 1 ]]; then
  if [[ ! -x "${lens_bin}" ]]; then
    log "install-mcp.sh: lens binary not found at ${lens_bin}; skipping MCP wire-up."
    log "install-mcp.sh: re-run after \`./scripts/install-lens.sh\` or pass --lens-bin <path>."
    exit 0
  fi
fi

# --- The Python merge. ---
#
# Operation: read claude.json (default {} if missing), set or remove the
# mcpServers.lens entry, write atomically. Unicode-safe via ensure_ascii=False.
# Output is pretty-printed with 2-space indent (matches claude.json's style).
python3 - "$claude_json" "$lens_bin" "$remove" "$dry_run" <<'PYEOF'
import json
import os
import sys
import tempfile
import time

claude_json_path = sys.argv[1]
lens_bin = sys.argv[2]
remove = sys.argv[3] == "1"
dry_run = sys.argv[4] == "1"

# Read existing config — default to empty object if missing or malformed
# (we'd rather refuse to corrupt a real config than silently create one,
# so malformed = abort with a clear error).
existing = {}
existed_before = os.path.exists(claude_json_path)
if existed_before:
    try:
        with open(claude_json_path, "r", encoding="utf-8") as f:
            existing = json.load(f)
        if not isinstance(existing, dict):
            print(
                f"install-mcp.sh: {claude_json_path} is not a JSON object (got {type(existing).__name__}); refusing to merge.",
                file=sys.stderr,
            )
            sys.exit(1)
    except json.JSONDecodeError as e:
        print(
            f"install-mcp.sh: {claude_json_path} is not valid JSON: {e}. Refusing to overwrite.",
            file=sys.stderr,
        )
        sys.exit(1)

# Compute the desired state.
mcp_servers = existing.get("mcpServers")
if mcp_servers is not None and not isinstance(mcp_servers, dict):
    print(
        f"install-mcp.sh: existing mcpServers is not an object; refusing to merge.",
        file=sys.stderr,
    )
    sys.exit(1)

before_lens = (mcp_servers or {}).get("lens")
if remove:
    if mcp_servers is None or "lens" not in mcp_servers:
        print("install-mcp.sh: no mcpServers.lens entry to remove; nothing to do.")
        sys.exit(0)
    desired = dict(existing)
    new_mcp = dict(mcp_servers)
    new_mcp.pop("lens", None)
    if new_mcp:
        desired["mcpServers"] = new_mcp
    else:
        # Don't leave behind an empty mcpServers object.
        desired.pop("mcpServers", None)
else:
    target = {"command": lens_bin, "args": ["mcp"]}
    if before_lens == target:
        print(f"install-mcp.sh: mcpServers.lens already up-to-date in {claude_json_path}. No action.")
        sys.exit(0)
    desired = dict(existing)
    new_mcp = dict(mcp_servers) if mcp_servers else {}
    new_mcp["lens"] = target
    desired["mcpServers"] = new_mcp

# Diff summary for the user — show what changed without dumping the entire file.
def short(v):
    return json.dumps(v, ensure_ascii=False)

if before_lens is None and not remove:
    print(f"install-mcp.sh: ADD mcpServers.lens = {short(desired['mcpServers']['lens'])}")
elif remove:
    print(f"install-mcp.sh: REMOVE mcpServers.lens (was {short(before_lens)})")
else:
    print(f"install-mcp.sh: UPDATE mcpServers.lens: {short(before_lens)} -> {short(desired['mcpServers']['lens'])}")

if dry_run:
    print("install-mcp.sh: --dry-run: no changes written.")
    sys.exit(0)

# Backup before writing — only if the file existed.
if existed_before:
    backup = f"{claude_json_path}.bak.{time.strftime('%Y%m%d-%H%M%S')}"
    with open(claude_json_path, "rb") as src, open(backup, "wb") as dst:
        dst.write(src.read())
    print(f"install-mcp.sh: backed up to {backup}")

# Atomic write: temp file in the same dir, then rename.
out_dir = os.path.dirname(os.path.abspath(claude_json_path)) or "."
fd, tmp = tempfile.mkstemp(prefix=".claude.json.staging.", dir=out_dir, text=True)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(desired, f, indent=2, ensure_ascii=False)
        f.write("\n")
    os.replace(tmp, claude_json_path)
except Exception:
    # Best-effort cleanup of the temp file if rename failed.
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise

print(f"install-mcp.sh: wrote {claude_json_path}")
if not remove:
    print("install-mcp.sh: restart Claude Code to pick up the new MCP server.")
PYEOF
