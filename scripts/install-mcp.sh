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
#   scripts/install-mcp.sh --version               # print version and exit
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
      sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    --version)
      sc_version
      exit 0 ;;
    *) echo "install-mcp.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

sc_assert_not_root "${EUID}" "${allow_root}" "install-mcp.sh" || exit 1

if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${lens_bin}"    "${HOME}" "install-mcp.sh" || exit 1
  sc_assert_strict_allowed "${claude_json}" "${HOME}" "install-mcp.sh" || exit 1
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

# --- The merge -------------------------------------------------------------
#
# Delegated to scripts/_json_edit.{py,js} via sc_json_edit, which reads the file
# (default {} when missing), refuses outright if it is malformed rather than
# overwriting a config it could not parse, and writes atomically via a sibling
# tmp file plus rename.
#
# This used to be an inline python3 heredoc gated on `command -v python3`. That
# gate passes on Windows, where the App Execution Alias stubs for python/python3
# sit on $PATH, print "Python was not found" and exit 49 — so the install died
# mid-surgery on exactly the machines least likely to have a real Python.
# sc_json_runtime probes by executing a trivial program and falls back to node.

target="{\"command\":\"${lens_bin}\",\"args\":[\"mcp\"]}"

before="$(sc_json_edit "${claude_json}" get mcpServers.lens 2>/dev/null || true)"

# Probe before logging, so an already-correct config reports "no action" rather
# than an "UPDATE x -> x" line that reads like a write happened.
if [[ "${remove}" == 1 ]]; then
  if [[ -z "${before}" ]]; then
    log "install-mcp.sh: no mcpServers.lens entry to remove; nothing to do."
    exit 0
  fi
  probe="$(sc_json_edit "${claude_json}" unset mcpServers.lens --dry-run)" || exit 1
else
  probe="$(sc_json_edit "${claude_json}" set mcpServers.lens "${target}" --dry-run)" || exit 1
fi

if [[ "${probe}" == "NOCHANGE" ]]; then
  log "install-mcp.sh: mcpServers.lens already up-to-date in ${claude_json}. No action."
  exit 0
fi

if [[ "${remove}" == 1 ]]; then
  log "install-mcp.sh: REMOVE mcpServers.lens (was ${before})"
elif [[ -z "${before}" ]]; then
  log "install-mcp.sh: ADD mcpServers.lens = ${target}"
else
  log "install-mcp.sh: UPDATE mcpServers.lens: ${before} -> ${target}"
fi

if [[ "${dry_run}" == 1 ]]; then
  log "install-mcp.sh: --dry-run: no changes written."
  exit 0
fi

# Backup before writing — only when the file existed. A backup of a file we are
# about to create from scratch is noise.
if [[ -f "${claude_json}" ]]; then
  backup="${claude_json}.bak.$(date '+%Y%m%d-%H%M%S')"
  cp -p "${claude_json}" "${backup}"
  log "install-mcp.sh: backed up to ${backup}"
fi

if [[ "${remove}" == 1 ]]; then
  sc_json_edit "${claude_json}" unset mcpServers.lens >/dev/null || exit 1
else
  sc_json_edit "${claude_json}" set mcpServers.lens "${target}" >/dev/null || exit 1
fi

log "install-mcp.sh: wrote ${claude_json}"
if [[ "${remove}" != 1 ]]; then
  log "install-mcp.sh: restart Claude Code to pick up the new MCP server."
fi
