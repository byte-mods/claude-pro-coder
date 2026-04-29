#!/usr/bin/env bash
# Uninstall the super-coder skill from ~/.claude/skills/super-coder.
#
# Usage:
#   scripts/uninstall.sh                  # remove ~/.claude/skills/super-coder
#   scripts/uninstall.sh --dest /custom   # custom skills root (default: ~/.claude/skills)
#   scripts/uninstall.sh --dry-run        # print what would be removed, don't remove
#   scripts/uninstall.sh --quiet          # suppress non-error output
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
dry_run=0
quiet=0

require_value() {
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
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
    --dest)    require_value "--dest" "${2:-}"; dest_root="$2"; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    --quiet)   quiet=1;   shift ;;
    -h|--help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "uninstall.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Unsafe-destination guard — implementation lives in scripts/_lib.sh so install.sh
# and uninstall.sh share one list. Refuses empty / "/", $HOME directly, and any
# system path on macOS or Linux.
sc_assert_safe_dest "${dest_root}" "uninstall.sh" || exit 1

dest="${dest_root}/super-coder"

# Defence-in-depth: even if dest_root passed the guard, refuse to remove a path
# whose final segment isn't `super-coder`. Closes a typo-injection style foot-gun
# (e.g. a future caller passing dest_root=/Users/me/important by mistake — the
# concatenation only ever appends /super-coder so this is mostly a sanity check).
case "${dest}" in
  */super-coder ) ;;
  * )
    echo "uninstall.sh: computed dest '${dest}' does not end in /super-coder. Refusing." >&2
    exit 1 ;;
esac

# Nothing to do?
if [[ ! -e "${dest}" ]] && [[ ! -L "${dest}" ]]; then
  log "uninstall.sh: nothing to remove at ${dest}. (Already uninstalled.)"
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
