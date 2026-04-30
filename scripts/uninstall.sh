#!/usr/bin/env bash
# Uninstall the pro-coder skill from ~/.claude/skills/pro-coder.
# Also removes the bundled `lens` binary at ~/.claude/bin/lens (and its
# install marker) and the `mcpServers.lens` entry from ~/.claude.json by
# default. Pass --keep-lens to leave the binary in place; --keep-mcp to leave
# the claude.json entry.
#
# Usage:
#   scripts/uninstall.sh                  # remove skill + lens binary + MCP entry
#   scripts/uninstall.sh --dest /custom   # custom skills root (default: ~/.claude/skills)
#   scripts/uninstall.sh --bin-dir DIR    # custom lens bin dir (default: ~/.claude/bin)
#   scripts/uninstall.sh --claude-json P  # custom claude.json (default: ~/.claude.json)
#   scripts/uninstall.sh --keep-lens      # leave the lens binary installed
#   scripts/uninstall.sh --keep-mcp       # leave mcpServers.lens in claude.json
#   scripts/uninstall.sh --dry-run        # print what would be removed, don't remove
#   scripts/uninstall.sh --quiet          # suppress non-error output
#   scripts/uninstall.sh --strict         # extra paranoia — refuse paths outside ~/.claude/
#   scripts/uninstall.sh --allow-root     # opt in to running as root (default: refuse)
#   scripts/uninstall.sh --version         # print version and exit
#
# Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE` forms.
#
# Idempotent: re-running when nothing is installed exits 0 with a no-op message.
# Refuses to operate outside the user's expected skills directory.

set -euo pipefail

# Source shared helpers — single source of truth for unsafe-dest list and HOME default.
# Use BASH_SOURCE not $0 so resolution survives a symlinked invocation.
_sc_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_lib.sh
. "${_sc_script_dir}/_lib.sh"

sc_set_default_home

dest_root="${HOME}/.claude/skills"
lens_bin_dir="${HOME}/.claude/bin"
claude_json="${HOME}/.claude.json"
keep_lens=0
keep_mcp=0
dry_run=0
quiet=0
strict=0
allow_root=0

require_value() {
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "uninstall.sh: $1 requires a value" >&2
    exit 2
  fi
}

require_eq_value() {
  # require_eq_value <flag-with-equals-prefix> <value-after-equals>
  # The `--flag=` form is rejected when the value is empty — same contract as
  # require_value rejecting a missing trailing arg.
  if [[ -z "${2:-}" ]]; then
    echo "uninstall.sh: $1 requires a value" >&2
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
    --dest)          require_value "--dest" "${2:-}"; dest_root="$2"; shift 2 ;;
    --dest=*)        require_eq_value "--dest=" "${1#--dest=}"; dest_root="${1#--dest=}"; shift ;;
    --bin-dir)       require_value "--bin-dir" "${2:-}"; lens_bin_dir="$2"; shift 2 ;;
    --bin-dir=*)     require_eq_value "--bin-dir=" "${1#--bin-dir=}"; lens_bin_dir="${1#--bin-dir=}"; shift ;;
    --claude-json)   require_value "--claude-json" "${2:-}"; claude_json="$2"; shift 2 ;;
    --claude-json=*) require_eq_value "--claude-json=" "${1#--claude-json=}"; claude_json="${1#--claude-json=}"; shift ;;
    --keep-lens)    keep_lens=1; shift ;;
    --keep-mcp)     keep_mcp=1; shift ;;
    --dry-run)      dry_run=1; shift ;;
    --quiet)        quiet=1;   shift ;;
    --strict)       strict=1;  shift ;;
    --allow-root)   allow_root=1; shift ;;
    -h|--help)
      sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    --version)
      sc_version
      exit 0 ;;
    *) echo "uninstall.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Refuse to run as root by default. --allow-root opts in (with a warning).
sc_assert_not_root "${EUID}" "${allow_root}" "uninstall.sh" || exit 1

# Unsafe-destination guard — implementation lives in scripts/_lib.sh so install.sh
# and uninstall.sh share one list. Refuses empty / "/", $HOME directly, and any
# system path on macOS or Linux.
sc_assert_safe_dest "${dest_root}" "uninstall.sh" || exit 1

# Strict mode (opt-in): also require every operative path to live under
# ${HOME}/.claude/. Layered on top of sc_assert_safe_dest.
if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${dest_root}"   "${HOME}" "uninstall.sh" || exit 1
  sc_assert_strict_allowed "${lens_bin_dir}" "${HOME}" "uninstall.sh" || exit 1
  sc_assert_strict_allowed "${claude_json}"  "${HOME}" "uninstall.sh" || exit 1
fi

dest="${dest_root}/pro-coder"

# Defence-in-depth: even if dest_root passed the guard, refuse to remove a path
# whose final segment isn't `pro-coder`. Closes a typo-injection style foot-gun
# (e.g. a future caller passing dest_root=/Users/me/important by mistake — the
# concatenation only ever appends /pro-coder so this is mostly a sanity check).
case "${dest}" in
  */pro-coder ) ;;
  * )
    echo "uninstall.sh: computed dest '${dest}' does not end in /pro-coder. Refusing." >&2
    exit 1 ;;
esac

# Reap orphan staging directories from interrupted prior installs. install.sh's
# copy mode stages into `${dest_root}/.pro-coder.staging.XXXXXX` (mktemp's
# 6-char alphanum pattern) and an EXIT trap removes it on graceful exit. A
# SIGKILL'd install leaves the staging dir behind — dead weight that clutters
# the skills root. Reap them here, with the same defence-in-depth tail check
# we applied to the main dest.
#
# Runs BEFORE the early-no-op return below so orphans are cleaned up even when
# no current install exists at ${dest}.
orphans_reaped=0
if [[ -d "${dest_root}" ]]; then
  while IFS= read -r staging; do
    [[ -z "${staging}" ]] && continue
    case "${staging}" in
      */.pro-coder.staging.* ) ;;
      * ) continue ;;  # defence-in-depth — must match the staging prefix
    esac
    if [[ "${dry_run}" == 1 ]]; then
      log "uninstall.sh: --dry-run: would reap orphan staging dir ${staging}"
    else
      log "uninstall.sh: reaping orphan staging dir ${staging}"
      rm -rf "${staging}"
    fi
    orphans_reaped=$((orphans_reaped + 1))
  done < <(find "${dest_root}" -maxdepth 1 -type d -name '.pro-coder.staging.*' 2>/dev/null)
fi

# Nothing to do at the main dest? The reap above may still have done useful
# work — if so, surface that instead of falsely claiming "already uninstalled".
if [[ ! -e "${dest}" ]] && [[ ! -L "${dest}" ]]; then
  if [[ "${orphans_reaped}" -gt 0 ]]; then
    log "uninstall.sh: reaped ${orphans_reaped} orphan staging dir(s); nothing else at ${dest}."
  else
    log "uninstall.sh: nothing to remove at ${dest}. (Already uninstalled.)"
  fi
  exit 0
fi

# What kind of thing are we removing? Surface it so the user sees what's about
# to disappear.
if [[ -L "${dest}" ]]; then
  target="$(readlink "${dest}")"
  log "uninstall.sh: removing symlink ${dest} -> ${target}"
elif [[ -d "${dest}" ]]; then
  log "uninstall.sh: removing directory ${dest}"
else
  log "uninstall.sh: removing ${dest}"
fi

if [[ "${dry_run}" == 1 ]]; then
  log "uninstall.sh: --dry-run: no changes made."
  exit 0
fi

# `rm -rf` on a symlink removes the link, not the target. On a directory it
# recursively removes contents. Both branches converge to "dest no longer exists".
rm -rf "${dest}"

if [[ -e "${dest}" || -L "${dest}" ]]; then
  echo "uninstall.sh: ${dest} still exists after rm. Investigate." >&2
  exit 1
fi

log "uninstall.sh: removed ${dest}."

# Lens binary cleanup. Defaults to removing; --keep-lens leaves it in place
# (useful when the user has other tooling that depends on lens being on PATH).
if [[ "${keep_lens}" == 1 ]]; then
  log "uninstall.sh: --keep-lens passed; left lens binary at ${lens_bin_dir}/lens."
else
  sc_assert_safe_dest "${lens_bin_dir}" "uninstall.sh" || exit 1
  lens_bin="${lens_bin_dir}/lens"
  lens_marker="${lens_bin_dir}/.lens.installed.sha"
  for path in "${lens_bin}" "${lens_marker}"; do
    if [[ -e "${path}" || -L "${path}" ]]; then
      log "uninstall.sh: removing ${path}"
      [[ "${dry_run}" == 1 ]] || rm -f "${path}"
    fi
  done
fi

# MCP entry cleanup. install-mcp.sh --remove handles backup + atomic rewrite.
# Skipped silently if claude.json doesn't exist or has no mcpServers.lens.
if [[ "${keep_mcp}" == 1 ]]; then
  log "uninstall.sh: --keep-mcp passed; left mcpServers.lens entry in ${claude_json}."
elif [[ -f "${claude_json}" ]]; then
  mcp_args=(--claude-json "${claude_json}" --remove)
  if [[ "${dry_run}" == 1 ]]; then
    mcp_args+=(--dry-run)
  fi
  if [[ "${quiet}" == 1 ]]; then
    mcp_args+=(--quiet)
  fi
  if [[ "${strict}" == 1 ]]; then
    mcp_args+=(--strict)
  fi
  if [[ "${allow_root}" == 1 ]]; then
    mcp_args+=(--allow-root)
  fi
  if ! "${_sc_script_dir}/install-mcp.sh" "${mcp_args[@]}"; then
    echo "uninstall.sh: WARNING — install-mcp.sh --remove failed; mcpServers.lens entry may still be in ${claude_json}." >&2
  fi
fi
