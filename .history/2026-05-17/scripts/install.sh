#!/usr/bin/env bash
# Install the shipped skills (pro-coder, diagram) into ~/.claude/skills/.
#
# By default, also (a) builds the bundled `lens` binary under ~/.claude/bin and
# (b) registers it as an MCP server in ~/.claude.json so Claude Code calls
# lens verbs as structured tools rather than via Bash. Pass --no-lens to skip
# the build, --no-mcp to skip the MCP wire-up.
#
# Usage:
#   scripts/install.sh                   # skill + lens + MCP wire-up (default)
#   scripts/install.sh --symlink         # symlink skill (recommended for developers)
#   scripts/install.sh --copy            # explicit copy mode
#   scripts/install.sh --dest /custom    # override destination root (default: ~/.claude/skills)
#   scripts/install.sh --dest=/custom    # equivalent --flag=VALUE form
#   scripts/install.sh --force           # overwrite existing destination + force lens rebuild
#   scripts/install.sh --no-lens         # skip building the lens binary (and MCP)
#   scripts/install.sh --no-mcp          # build lens but skip the ~/.claude.json wire-up
#   scripts/install.sh --bin-dir DIR     # custom lens binary dest (default: ~/.claude/bin)
#   scripts/install.sh --claude-json P   # custom claude.json (default: ~/.claude.json)
#   scripts/install.sh --dry-run         # print what would be done; make no changes
#   scripts/install.sh --quiet           # suppress non-error output
#   scripts/install.sh --strict          # extra paranoia — refuse paths outside ~/.claude/
#   scripts/install.sh --allow-root      # opt in to running as root (default: refuse)
#   scripts/install.sh --version          # print version and exit
#
# Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE` forms.
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
install_lens=1
install_mcp=1
lens_bin_dir="${HOME}/.claude/bin"
claude_json="${HOME}/.claude.json"
dry_run=0
quiet=0
strict=0
allow_root=0

require_value() {
  # require_value <flag> <value>
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "install.sh: $1 requires a value" >&2
    exit 2
  fi
}

require_eq_value() {
  # require_eq_value <flag-with-equals-prefix> <value-after-equals>
  # The `--flag=` form is rejected when the value is empty — same contract as
  # require_value rejecting a missing trailing arg.
  if [[ -z "${2:-}" ]]; then
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

log() {
  # Honours --quiet. Errors do NOT go through log() — they go straight to
  # stderr so a quiet install still surfaces failures.
  if [[ "${quiet}" != 1 ]]; then
    echo "$@"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --symlink) set_mode "symlink"; shift ;;
    --copy)    set_mode "copy";    shift ;;
    --dest)         require_value "--dest" "${2:-}"; dest_root="$2"; shift 2 ;;
    --dest=*)       require_eq_value "--dest=" "${1#--dest=}"; dest_root="${1#--dest=}"; shift ;;
    --force)   force=1;        shift ;;
    --no-lens) install_lens=0; install_mcp=0; shift ;;
    --no-mcp)  install_mcp=0; shift ;;
    --bin-dir)      require_value "--bin-dir" "${2:-}"; lens_bin_dir="$2"; shift 2 ;;
    --bin-dir=*)    require_eq_value "--bin-dir=" "${1#--bin-dir=}"; lens_bin_dir="${1#--bin-dir=}"; shift ;;
    --claude-json)  require_value "--claude-json" "${2:-}"; claude_json="$2"; shift 2 ;;
    --claude-json=*) require_eq_value "--claude-json=" "${1#--claude-json=}"; claude_json="${1#--claude-json=}"; shift ;;
    --dry-run) dry_run=1; shift ;;
    --quiet)   quiet=1;   shift ;;
    --strict)  strict=1;  shift ;;
    --allow-root) allow_root=1; shift ;;
    -h|--help)
      sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    --version)
      sc_version
      exit 0 ;;
    *) echo "install.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Refuse to run as root by default. --allow-root opts in (with a warning) for
# CI containers and other deliberate cases.
sc_assert_not_root "${EUID}" "${allow_root}" "install.sh" || exit 1

# Default mode after arg-parse so --copy/--symlink mutual exclusion can be enforced
# without the default biasing the check.
[[ -z "${mode}" ]] && mode="copy"

# Resolve the repo root relative to this script — works whether the user runs
# `scripts/install.sh` from the repo root or from inside scripts/.
# Reuse _sc_script_dir from the source-block above (already absolute, BASH_SOURCE-based).
script_dir="${_sc_script_dir}"
repo_root="$(cd "${script_dir}/.." && pwd)"

# Skills shipped in this repo. Each entry names a directory under the repo root
# that contains a SKILL.md file. Add new skills here; the install loop handles
# the rest.
skill_names=("pro-coder" "diagram")

# Validate all skill sources exist before touching the filesystem.
for skill_name in "${skill_names[@]}"; do
  src="${repo_root}/${skill_name}"
  if [[ ! -f "${src}/SKILL.md" ]]; then
    echo "install.sh: cannot find SKILL.md at ${src}/SKILL.md" >&2
    echo "install.sh: this script must live at <repo>/scripts/install.sh" >&2
    exit 1
  fi
done

# Unsafe-destination guard — implementation lives in scripts/_lib.sh so install.sh
# and uninstall.sh share one list. Refuses empty / "/", $HOME directly, and any
# system path on macOS or Linux.
sc_assert_safe_dest "${dest_root}" "install.sh" || exit 1

# Strict mode (opt-in): also require dest_root, lens_bin_dir, claude_json to be
# under ${HOME}/.claude/. Layered on top of the unsafe-dest guard, not replacing
# it. Useful when paths are sourced from a config file the user does not fully
# control.
if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${dest_root}"   "${HOME}" "install.sh" || exit 1
  sc_assert_strict_allowed "${lens_bin_dir}" "${HOME}" "install.sh" || exit 1
  sc_assert_strict_allowed "${claude_json}"  "${HOME}" "install.sh" || exit 1
fi

if [[ "${dry_run}" == 1 ]]; then
  log "install.sh: --dry-run: would mkdir -p ${dest_root}"
else
  mkdir -p "${dest_root}"
fi

# Install each skill. Same logic for all: idempotency check, copy or symlink,
# verify. The loop runs per-skill so a missing source for one skill doesn't
# affect the others (sources already validated above).
for skill_name in "${skill_names[@]}"; do
  src="${repo_root}/${skill_name}"
  dest="${dest_root}/${skill_name}"
  staging_prefix=".${skill_name}.staging"

  # Idempotency: if dest already matches what we'd install, skip this skill.
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
    log "install.sh: ${dest} already up-to-date (mode=${mode}). Skipping skill copy."
    continue
  fi

  # Existing destination handling. We use atomic rename for copy mode and ln -sfn
  # for symlink mode so the destination swap is not interruptible — closes the
  # TOCTOU window between rm and cp/ln.
  if [[ -e "${dest}" ]] || [[ -L "${dest}" ]]; then
    if [[ "${force}" != 1 ]]; then
      echo "install.sh: ${dest} already exists. Re-run with --force to overwrite." >&2
      exit 1
    fi
  fi

  if [[ "${dry_run}" == 1 ]]; then
    if [[ "${mode}" == "symlink" ]]; then
      log "install.sh: --dry-run: would symlink ${dest} -> ${src}"
    else
      log "install.sh: --dry-run: would copy ${src} -> ${dest} (atomic rename via staging dir)"
    fi
  elif [[ "${mode}" == "symlink" ]]; then
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
    log "install.sh: symlinked ${dest} -> ${src}"
  else
    # Stage into a sibling tmp dir, then rename. mv on the same filesystem is
    # atomic on POSIX. -RP preserves symlinks inside the source rather than
    # following them — guards against an attacker-controlled symlink loop.
    staging="$(mktemp -d "${dest_root}/${staging_prefix}.XXXXXX")"
    trap 'rm -rf "${staging}"' EXIT
    cp -RP "${src}/." "${staging}/"
    if [[ -e "${dest}" || -L "${dest}" ]]; then
      rm -rf "${dest}"
    fi
    mv "${staging}" "${dest}"
    trap - EXIT
    log "install.sh: copied ${src} -> ${dest}"
  fi

  # Surface the verify step so the user knows the install landed.
  if [[ "${dry_run}" != 1 ]]; then
    if [[ -f "${dest}/SKILL.md" ]]; then
      log "install.sh: verified ${dest}/SKILL.md exists. Skill is installed."
    else
      echo "install.sh: WARNING — ${dest}/SKILL.md not found after install. Investigate." >&2
      exit 1
    fi
  fi
done

# Build + install the bundled lens binary unless explicitly skipped. install-lens.sh
# handles cargo-not-installed gracefully (warns + exits 0), but in v6 the skill
# requires lens — pro-coder will refuse to run at first invocation if the binary
# is not on $PATH. The graceful install exit lets the user install Rust later
# and re-run install.sh without manual cleanup; the skill activates once lens is
# present.
if [[ "${install_lens}" == 1 ]]; then
  if [[ "${dry_run}" == 1 ]]; then
    log "install.sh: --dry-run: would build lens via install-lens.sh --bin-dir ${lens_bin_dir} (cargo build skipped under --dry-run)"
  else
    lens_args=(--bin-dir "${lens_bin_dir}")
    if [[ "${force}" == 1 ]]; then
      lens_args+=(--force)
    fi
    if [[ "${quiet}" == 1 ]]; then
      lens_args+=(--quiet)
    fi
    if [[ "${strict}" == 1 ]]; then
      lens_args+=(--strict)
    fi
    if [[ "${allow_root}" == 1 ]]; then
      lens_args+=(--allow-root)
    fi
    log "install.sh: building bundled lens (pass --no-lens to skip)"
    if ! "${script_dir}/install-lens.sh" "${lens_args[@]}"; then
      echo "install.sh: WARNING — install-lens.sh failed. pro-coder requires lens in v6 and will REFUSE to run at first invocation. Fix the build error and re-run install.sh." >&2
      # Do not fail the overall install — the user may want to install lens by hand.
    fi
  fi
else
  log "install.sh: --no-lens passed; skipped lens build. pro-coder requires lens in v6 and will REFUSE to run at first invocation. Install lens later via 'scripts/install-lens.sh' to enable the skill."
fi

# Wire the lens MCP server into ~/.claude.json so Claude Code spawns it at
# startup. This is the difference between "Claude shells out to lens via Bash"
# (still works without this) and "Claude calls lens tools directly" (the
# token-efficient surfing path). install-mcp.sh is idempotent and refuses to
# write a broken entry if the lens binary is missing.
if [[ "${install_mcp}" == 1 ]] && [[ "${install_lens}" == 1 ]]; then
  mcp_args=(--lens-bin "${lens_bin_dir}/lens" --claude-json "${claude_json}")
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
  log "install.sh: registering lens as an MCP server in ${claude_json} (pass --no-mcp to skip)"
  if ! "${script_dir}/install-mcp.sh" "${mcp_args[@]}"; then
    echo "install.sh: WARNING — install-mcp.sh failed. lens still works via Bash CLI; only the MCP integration is missing." >&2
  fi
elif [[ "${install_mcp}" == 0 ]]; then
  log "install.sh: --no-mcp passed; skipped ~/.claude.json wire-up. lens still works via Bash CLI."
fi

if [[ "${dry_run}" == 1 ]]; then
  log "install.sh: --dry-run: no changes made."
fi
