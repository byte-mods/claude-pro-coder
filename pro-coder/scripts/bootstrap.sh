#!/usr/bin/env bash
# pro-coder bootstrap — the mandatory first action of every P1.
#
# Bootstrap used to be five prose steps in the middle of SKILL.md. Prose steps
# get half-performed: the observed failure was an agent that created
# .claude/state/ and .history/, then skipped current-tasks.md and the lens index
# entirely and went straight to the phases. One script is one Bash call; there
# is no "did four of five" outcome.
#
# Deliberately standalone: no `. _lib.sh`. The skill installs to
# ~/.claude/skills/pro-coder/ without the repo's scripts/ directory, so this
# file cannot depend on it. (The _lib.sh sourcing invariant in CLAUDE.md governs
# scripts/ — the install pipeline — not the skill payload.)
#
# Usage:
#   bash scripts/bootstrap.sh            # run from the project root
#   bash scripts/bootstrap.sh --quiet    # machine-readable report only
#
# Exit 0 — bootstrap complete; the report names anything the agent must act on.
# Exit 1 — ABORT. stdout carries the exact error string the protocol requires;
#          the loop must stop, not continue in a degraded mode.

set -uo pipefail

quiet=0
for arg in "$@"; do
  case "${arg}" in
    --quiet) quiet=1 ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bootstrap.sh: unknown arg: ${arg}" >&2; exit 2 ;;
  esac
done

say() { [[ "${quiet}" == 1 ]] || echo "$@"; }

# Project root: git's answer when there is one, else the working directory.
if root="$(git rev-parse --show-toplevel 2>/dev/null)" && [[ -n "${root}" ]]; then
  cd "${root}" || { echo "bootstrap.sh: cannot cd to ${root}" >&2; exit 1; }
else
  root="${PWD}"
fi

notes=()
note() { notes+=( "$1" ); }

# --- Step 1: state directories + mandatory ledger files --------------------
#
# .history/ and current-tasks.md are load-bearing: the protocol aborts rather
# than run without them, because a missing ledger means silent data loss rather
# than a visible error.

if ! mkdir -p .claude/state .claude/state/code-map 2>/dev/null; then
  echo "> ABORT: cannot create .claude/state/ — mkdir failed. The skill requires the state directory to operate. Fix the underlying issue and re-invoke."
  exit 1
fi

if [[ ! -d .history ]]; then
  if ! mkdir -p .history 2>/dev/null; then
    echo "> ABORT: cannot create .history/ — mkdir failed (permission denied, read-only filesystem, or disk full). The skill requires the file-history archive to operate. Fix the underlying issue and re-invoke."
    exit 1
  fi
  note "created .history/"
fi

if [[ ! -f current-tasks.md ]]; then
  if ! cat > current-tasks.md <<'LEDGER'
# Current Tasks

_Single source of truth for in-flight work. Updated by the agent before starting and after completing every task. Read this first to see what is in progress, what is queued, and what is done._

## In progress

_(none)_

## Queued

_(none)_

## Completed (this session)

_(none)_
LEDGER
  then
    rm -f current-tasks.md 2>/dev/null
    echo "> ABORT: cannot create current-tasks.md — write failed (permission denied, read-only filesystem, or disk full). The skill requires the in-flight task ledger to operate. Fix the underlying issue and re-invoke."
    exit 1
  fi
  note "created current-tasks.md from the header template"
fi

# --- Step 2: gitignore policy (ask-once) -----------------------------------

gitignore_policy="unset"
if [[ -f .claude/state/gitignore_policy ]]; then
  gitignore_policy="$(tr -d '[:space:]' < .claude/state/gitignore_policy)"
else
  note "ACTION REQUIRED: no gitignore policy recorded. Ask the user the Bootstrap Step 2 question verbatim, then write 'ignore' or 'commit' to .claude/state/gitignore_policy."
fi

# --- Steps 3, 4, 1b: presence checks (report only; never auto-create) ------

claude_md="absent"
[[ -f CLAUDE.md ]] && claude_md="present"
[[ "${claude_md}" == "absent" ]] && note "no CLAUDE.md found — surface the Step 3 note once, then proceed. Never create it."

code_map_count="$(find .claude/state/code-map -maxdepth 1 -name '*.md' 2>/dev/null | wc -l | tr -d ' ')"
[[ "${code_map_count}" == "0" ]] && note "code-map is empty — normal on first invocation; it fills as P5 cycles close."

db_project="no"
for marker in schema.txt migrations prisma db sql; do
  [[ -e "${marker}" ]] && db_project="yes" && break
done
if [[ "${db_project}" == "no" ]]; then
  # Second pass: model/schema files anywhere shallow in the tree.
  if find . -maxdepth 3 -not -path './.git/*' \( -iname '*schema*' -o -iname '*model*' \) 2>/dev/null | grep -q .; then
    db_project="yes"
  fi
fi
if [[ "${db_project}" == "yes" && ! -f schema.txt ]]; then
  note "database detected but schema.txt missing — surface the Step 1b note once; create schema.txt on the first schema change."
fi

# --- Step 5: lens index (REQUIRED — no fallback) ---------------------------

if ! command -v lens >/dev/null 2>&1; then
  echo "> ABORT: lens binary not found on \$PATH. The skill requires lens to operate — there is no fallback. Install lens by re-running the claude-skill installer (\`./scripts/install.sh\` from the claude-skill repo) or by building it from source (https://github.com/sudeep-dasgupta/lens). Then re-invoke the skill."
  exit 1
fi

lens_action=""
lens_output=""
if [[ -f .lens/index.db ]]; then
  lens_action="update"
  lens_output="$(lens update 2>&1)" || lens_output="lens update failed: ${lens_output}"
else
  lens_action="index"
  lens_output="$(lens init 2>&1 && lens index 2>&1)" || lens_output="lens index failed: ${lens_output}"
fi

if [[ ! -f .lens/index.db ]]; then
  echo "> ABORT: lens ran but .lens/index.db was not created. Output follows — fix the underlying issue and re-invoke."
  echo "${lens_output}"
  exit 1
fi

# A zero-symbol index is legitimate (unsupported language, or greenfield). It is
# explicitly NOT a fallback mode: lens is present and `lens search` still serves
# every lookup over the full-text index.
if printf '%s' "${lens_output}" | grep -qE '(^|[^0-9])0 symbols'; then
  note "lens indexed 0 symbols (no supported-language files, or greenfield). Symbol verbs return empty slices — use 'lens search' for every lookup and targeted Read ranges. This is not a fallback; the lens contract is met."
fi

# --- Guard marker ----------------------------------------------------------
#
# hooks/lens_guard.sh stays inert until this marker exists, which is what keeps
# the Grep/Read guard scoped to projects pro-coder actually drives rather than
# firing in every Claude Code session on the machine.

printf 'pro-coder guard active. Delete this file to disable the lens-first PreToolUse guard for this project.\n' \
  > .claude/state/pro-coder-guard 2>/dev/null \
  || note "WARNING: could not write .claude/state/pro-coder-guard — the lens-first guard will stay inert in this project."

# --- Report ----------------------------------------------------------------

say "pro-coder bootstrap — $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
say "  root:             ${root}"
say "  .history/:        present"
say "  current-tasks.md: present"
say "  code-map notes:   ${code_map_count}"
say "  CLAUDE.md:        ${claude_md}"
say "  gitignore policy: ${gitignore_policy}"
say "  database project: ${db_project}"
say "  lens:             $(lens --version 2>/dev/null || echo 'unknown') (${lens_action})"
say "  guard:            $([[ -f .claude/state/pro-coder-guard ]] && echo 'armed' || echo 'INERT')"
say ""
say "lens output:"
say "${lens_output}"

if [[ ${#notes[@]} -gt 0 ]]; then
  say ""
  say "notes (surface each of these once, then proceed):"
  for n in "${notes[@]}"; do
    say "  - ${n}"
  done
fi

say ""
say "BOOTSTRAP OK"
exit 0
