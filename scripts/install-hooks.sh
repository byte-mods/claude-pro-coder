#!/usr/bin/env bash
# Register the pro-coder lens-first PreToolUse hook in ~/.claude/settings.json.
#
# This is the mechanism half of the v8 change. SKILL.md has always *said* lens
# is required; prose does not survive context pressure, and the observed failure
# was an agent that followed the phase headings and reached for Grep/Read anyway.
# The hook blocks the cheap path so "lens first" holds without the model having
# to remember it.
#
# The hook itself is scoped so it stays out of the way: it is inert unless the
# project has BOTH a lens index and the pro-coder guard marker that
# pro-coder/scripts/bootstrap.sh writes. Registering it globally is therefore
# safe — ordinary Claude Code work in other projects is untouched.
#
# Usage:
#   scripts/install-hooks.sh                  # register the hook
#   scripts/install-hooks.sh --remove         # unregister it
#   scripts/install-hooks.sh --settings P     # custom settings.json (default: ~/.claude/settings.json)
#   scripts/install-hooks.sh --skills-dir DIR # where the skill was installed (default: ~/.claude/skills)
#   scripts/install-hooks.sh --dry-run        # print what would change; write nothing
#   scripts/install-hooks.sh --quiet          # suppress non-error output
#   scripts/install-hooks.sh --strict         # refuse paths outside ~/.claude/
#   scripts/install-hooks.sh --allow-root     # opt in to running as root
#   scripts/install-hooks.sh --version        # print version and exit
#
# Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE`.
# Idempotent: re-running with the same flags is a no-op.

set -euo pipefail

_sc_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_lib.sh
. "${_sc_script_dir}/_lib.sh"

sc_set_default_home

settings="${HOME}/.claude/settings.json"
skills_dir="${HOME}/.claude/skills"
remove=0
dry_run=0
quiet=0
strict=0
allow_root=0

# The event and matcher the hook binds to, and the substring used to find our
# own entry again on removal. The marker must stay stable across versions — it
# is how an upgrade finds and replaces the previous registration instead of
# stacking a second copy.
hook_event="PreToolUse"
hook_matcher="Grep|Read"
hook_marker="lens_guard.sh"

require_value() {
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "install-hooks.sh: $1 requires a value" >&2
    exit 2
  fi
}

require_eq_value() {
  if [[ -z "${2:-}" ]]; then
    echo "install-hooks.sh: $1 requires a value" >&2
    exit 2
  fi
}

log() {
  # Honours --quiet. Errors go straight to stderr, never through log().
  if [[ "${quiet}" != 1 ]]; then
    echo "$@"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --remove)         remove=1; shift ;;
    --settings)       require_value "--settings" "${2:-}"; settings="$2"; shift 2 ;;
    --settings=*)     require_eq_value "--settings=" "${1#--settings=}"; settings="${1#--settings=}"; shift ;;
    --skills-dir)     require_value "--skills-dir" "${2:-}"; skills_dir="$2"; shift 2 ;;
    --skills-dir=*)   require_eq_value "--skills-dir=" "${1#--skills-dir=}"; skills_dir="${1#--skills-dir=}"; shift ;;
    --dry-run)        dry_run=1; shift ;;
    --quiet)          quiet=1;   shift ;;
    --strict)         strict=1;  shift ;;
    --allow-root)     allow_root=1; shift ;;
    -h|--help)
      sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    --version)
      sc_version
      exit 0 ;;
    *) echo "install-hooks.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

sc_assert_not_root "${EUID}" "${allow_root}" "install-hooks.sh" || exit 1

sc_assert_safe_dest "${settings}"   "install-hooks.sh" || exit 1
sc_assert_safe_dest "${skills_dir}" "install-hooks.sh" || exit 1

if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${settings}"   "${HOME}" "install-hooks.sh" || exit 1
  sc_assert_strict_allowed "${skills_dir}" "${HOME}" "install-hooks.sh" || exit 1
fi

hook_script="${skills_dir}/pro-coder/hooks/${hook_marker}"

if [[ "${remove}" != 1 ]] && [[ ! -f "${hook_script}" ]]; then
  echo "install-hooks.sh: hook script not found at ${hook_script}." >&2
  echo "install-hooks.sh: install the skill first (scripts/install.sh), then re-run." >&2
  exit 1
fi

# --- Build the command string --------------------------------------------
#
# Claude Code executes the hook command through the platform shell. On Windows
# that is not git-bash: a bare `bash` there resolves to C:\Windows\System32\bash.exe
# (WSL), which cannot see the Windows-side paths this hook is given. So on
# MSYS/Cygwin/MinGW we bake in the absolute path of the git-bash that is running
# this installer and hand it a Windows-style path, both via cygpath.
bash_cmd="bash"
script_arg="${hook_script}"

case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW* | MSYS* | CYGWIN*)
    if command -v cygpath >/dev/null 2>&1; then
      # -m gives a Windows path with forward slashes: no backslash escaping to
      # get wrong when this string is embedded in JSON.
      bash_cmd="$(cygpath -m "$(command -v bash)")"
      script_arg="$(cygpath -m "${hook_script}")"
    fi
    ;;
esac

hook_command="\"${bash_cmd}\" \"${script_arg}\""

# --- Apply -----------------------------------------------------------------
#
# Backup first, then let the shared JSON editor do the merge atomically. The
# editor preserves every unrelated key and every hook the user registered
# themselves; it refuses outright if settings.json is malformed rather than
# overwriting a config it could not parse.

if [[ ! -f "${settings}" ]] && [[ "${remove}" == 1 ]]; then
  log "install-hooks.sh: ${settings} does not exist; nothing to remove."
  exit 0
fi

if [[ "${remove}" == 1 ]]; then
  log "install-hooks.sh: REMOVE hooks.${hook_event} entries matching '${hook_marker}' from ${settings}"
else
  log "install-hooks.sh: SET hooks.${hook_event}[matcher='${hook_matcher}'] = ${hook_command}"
fi

# Probe with --dry-run first so we know whether a backup is warranted. Writing a
# backup for a no-op change litters the user's .claude directory with copies.
if [[ "${remove}" == 1 ]]; then
  probe="$(sc_json_edit "${settings}" hook-remove "${hook_event}" "${hook_marker}" --dry-run)" || exit 1
else
  probe="$(sc_json_edit "${settings}" hook-upsert "${hook_event}" "${hook_matcher}" "${hook_command}" --dry-run)" || exit 1
fi

if [[ "${probe}" == "NOCHANGE" ]]; then
  log "install-hooks.sh: ${settings} already up-to-date. No action."
  exit 0
fi

if [[ "${dry_run}" == 1 ]]; then
  log "install-hooks.sh: --dry-run: no changes written."
  exit 0
fi

if [[ -f "${settings}" ]]; then
  backup="${settings}.bak.$(date '+%Y%m%d-%H%M%S')"
  cp -p "${settings}" "${backup}"
  log "install-hooks.sh: backed up to ${backup}"
fi

if [[ "${remove}" == 1 ]]; then
  sc_json_edit "${settings}" hook-remove "${hook_event}" "${hook_marker}" >/dev/null || exit 1
  log "install-hooks.sh: removed the lens-first guard from ${settings}"
else
  sc_json_edit "${settings}" hook-upsert "${hook_event}" "${hook_matcher}" "${hook_command}" >/dev/null || exit 1
  log "install-hooks.sh: wrote ${settings}"
  log "install-hooks.sh: the guard stays inert until pro-coder bootstraps a project"
  log "install-hooks.sh: (it needs both .lens/index.db and .claude/state/pro-coder-guard)."
  log "install-hooks.sh: restart Claude Code to pick up the new hook."
fi
