#!/usr/bin/env bash
# Install the super-coder skill into ~/.claude/skills/super-coder.
#
# Usage:
#   scripts/install.sh                  # copy mode (default; safe for end-users)
#   scripts/install.sh --symlink        # symlink mode (recommended for skill developers)
#   scripts/install.sh --copy           # explicit copy mode
#   scripts/install.sh --dest /custom   # override destination root (default: ~/.claude/skills)
#   scripts/install.sh --force          # overwrite existing destination without prompting
#
# Idempotent: re-running with the same flags is a no-op when source and destination match.

set -euo pipefail

# Source shared helpers — single source of truth for unsafe-dest list and HOME default.
# Use BASH_SOURCE not $0 so resolution survives a symlinked invocation.
_sc_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_lib.sh
. "${_sc_script_dir}/_lib.sh"

sc_set_default_home

mode=""
dest_root="${HOME}/.claude/skills"
force=0

require_value() {
  # require_value <flag> <value>
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "install.sh: $1 requires a value" >&2
    exit 2
  fi
}

set_mode() {
  if [[ -n "${mode}" ]] && [[ "${mode}" != "$1" ]]; then
    echo "install.sh: --copy and --symlink are mutually exclusive" >&2
    exit 2
  fi
  mode="$1"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --symlink) set_mode "symlink"; shift ;;
    --copy)    set_mode "copy";    shift ;;
    --dest)    require_value "--dest" "${2:-}"; dest_root="$2"; shift 2 ;;
    --force)   force=1;        shift ;;
    -h|--help)
      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "install.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Default mode after arg-parse so --copy/--symlink mutual exclusion can be enforced
# without the default biasing the check.
[[ -z "${mode}" ]] && mode="copy"

# Resolve the source skill directory relative to this script — works whether the
# user runs `scripts/install.sh` from the repo root or from inside scripts/.
# Reuse _sc_script_dir from the source-block above (already absolute, BASH_SOURCE-based).
script_dir="${_sc_script_dir}"
repo_root="$(cd "${script_dir}/.." && pwd)"
src="${repo_root}/super-coder"

if [[ ! -f "${src}/SKILL.md" ]]; then
  echo "install.sh: cannot find SKILL.md at ${src}/SKILL.md" >&2
  echo "install.sh: this script must live at <repo>/scripts/install.sh" >&2
  exit 1
fi

# Unsafe-destination guard — implementation lives in scripts/_lib.sh so install.sh
# and uninstall.sh share one list. Refuses empty / "/", $HOME directly, and any
# system path on macOS or Linux.
sc_assert_safe_dest "${dest_root}" "install.sh" || exit 1

dest="${dest_root}/super-coder"

mkdir -p "${dest_root}"

# Idempotency: if dest already matches what we'd install, exit 0 quietly.
already_correct=0
if [[ "${mode}" == "symlink" ]] && [[ -L "${dest}" ]]; then
  current_target="$(readlink "${dest}")"
  if [[ "${current_target}" == "${src}" ]]; then
    already_correct=1
  fi
elif [[ "${mode}" == "copy" ]] && [[ -d "${dest}" ]] && [[ ! -L "${dest}" ]] && [[ -f "${dest}/SKILL.md" ]]; then
  if cmp -s "${src}/SKILL.md" "${dest}/SKILL.md"; then
    already_correct=1
  fi
fi

if [[ "${already_correct}" == 1 ]]; then
  echo "install.sh: ${dest} already up-to-date (mode=${mode}). No action."
  exit 0
fi

# Existing destination handling. We use atomic rename for copy mode and ln -sfn for
# symlink mode so the destination swap is not interruptible — closes the TOCTOU
# window between rm and cp/ln.
if [[ -e "${dest}" || -L "${dest}" ]]; then
  if [[ "${force}" != 1 ]]; then
    echo "install.sh: ${dest} already exists. Re-run with --force to overwrite." >&2
    exit 1
  fi
fi

if [[ "${mode}" == "symlink" ]]; then
  # ln -sfn replaces an existing symlink atomically, but it does NOT replace a
  # real directory — it would create the link inside the dir. Remove first.
  if [[ -d "${dest}" ]] && [[ ! -L "${dest}" ]]; then
    rm -rf "${dest}"
  fi
  ln -sfn "${src}" "${dest}"
  # Verify dest is now a symlink pointing where we expect — defends against
  # the silent "link landed inside an existing dir" mode.
  if [[ ! -L "${dest}" ]] || [[ "$(readlink "${dest}")" != "${src}" ]]; then
    echo "install.sh: symlink at ${dest} did not land correctly. Investigate." >&2
    exit 1
  fi
  echo "install.sh: symlinked ${dest} -> ${src}"
else
  # Stage into a sibling tmp dir, then rename. mv on the same filesystem is
  # atomic on POSIX. -RP preserves symlinks inside the source rather than
  # following them — guards against an attacker-controlled symlink loop in
  # super-coder/.
  staging="$(mktemp -d "${dest_root}/.super-coder.staging.XXXXXX")"
  trap 'rm -rf "${staging}"' EXIT
  cp -RP "${src}/." "${staging}/"
  if [[ -e "${dest}" || -L "${dest}" ]]; then
    rm -rf "${dest}"
  fi
  mv "${staging}" "${dest}"
  trap - EXIT
  echo "install.sh: copied ${src} -> ${dest}"
fi

# Surface the verify step so the user knows the install landed.
if [[ -f "${dest}/SKILL.md" ]]; then
  echo "install.sh: verified ${dest}/SKILL.md exists. Skill is installed."
else
  echo "install.sh: WARNING — ${dest}/SKILL.md not found after install. Investigate." >&2
  exit 1
fi
