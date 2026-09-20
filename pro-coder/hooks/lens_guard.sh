#!/usr/bin/env bash
# pro-coder PreToolUse hook — makes "lens first" a mechanism instead of a wish.
#
# The SKILL.md protocol has always *said* lens is required. Prose in a large
# prompt loses to a one-call native tool under context pressure: the model
# reaches for Grep/Read because they are cheap, and the lens mandate quietly
# stops happening. This hook removes the cheap path.
#
# Contract (Claude Code PreToolUse):
#   stdin  — JSON: {cwd, tool_name, tool_input{...}, ...}
#   stdout — JSON with hookSpecificOutput.permissionDecision="deny" to block;
#            nothing at all to allow.
#   exit 0 — always. This hook FAILS OPEN: any parse error, any unexpected
#            shape, any missing tool is an allow. A guard that breaks the
#            session is worse than a guard that misses a case.
#
# Scope — deliberately narrow. The guard is inert unless BOTH hold:
#   1. `.lens/index.db` exists at or above cwd  (lens actually indexed here), AND
#   2. `.claude/state/pro-coder-guard` exists there (pro-coder bootstrapped here).
# Condition 2 is what keeps this out of the way of ordinary Claude Code work in
# every other project on the machine. Escape hatch: PRO_CODER_GUARD=0.
#
# What it blocks, and why each block is escapable:
#   Grep, unscoped         -> `lens search` is the ranked, budget-capped,
#                             symbol-annotated version of the same query. A Grep
#                             scoped to one existing file is ALLOWED — the
#                             protocol explicitly permits that for regexes lens
#                             search cannot express.
#   Read of a large file   -> only when offset and limit are both absent. Adding
#     with no offset/limit    either one, or using `lens slice`/`lens follow`,
#                             satisfies the guard. Ledger files the protocol
#                             mandates reading whole are exempt.
# Neither block is a dead end: every denial names the command to run instead.

set -uo pipefail

# Fail open on anything unexpected. Belt and braces alongside the explicit
# `allow` calls below: if a future edit introduces an error path, allow the tool.
trap 'exit 0' ERR

allow() { exit 0; }

case "${PRO_CODER_GUARD:-}" in
  0 | off | OFF | false) allow ;;
esac

payload="$(cat 2>/dev/null || true)"
[[ -z "${payload}" ]] && allow

# --- Minimal JSON field extraction ---------------------------------------
#
# No jq, no python: this runs on every Grep/Read call and must not depend on a
# runtime the machine might lack (the same Windows Store python-stub problem
# that sc_json_runtime works around). The fields we need are flat strings and
# integers, and every miss is an allow, so a hand-rolled reader is safe here.
#
# `.*` is greedy, so these pick the LAST match; each key appears at most once in
# a PreToolUse payload. Note that "file_path" does not collide with "path"
# because the literal searched for includes the opening quote.

json_str() {
  printf '%s' "${payload}" \
    | tr -d '\n' \
    | sed -n 's/.*"'"$1"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
}

json_has_num() {
  printf '%s' "${payload}" \
    | tr -d '\n' \
    | grep -qE "\"$1\"[[:space:]]*:[[:space:]]*[0-9]"
}

# Windows paths arrive JSON-escaped, e.g. C:\\Users\\me. Collapse every
# backslash to a forward slash — git-bash resolves C:/Users/me natively, the
# backslash form it does not. Pure parameter expansion rather than sed, whose
# backslash handling is a portability trap between BSD and GNU.
unwin() {
  # `dbl`/`one` are held in variables because the literal form of the collapse
  # -- ${s/////} -- is parsed ambiguously by bash and loops forever.
  local s="$1" dbl='//' one='/'
  s="${s//\\//}"
  while [[ "${s}" == *"${dbl}"* ]]; do s="${s//${dbl}/${one}}"; done
  printf '%s' "${s}"
}

tool_name="$(json_str tool_name)"
[[ -z "${tool_name}" ]] && allow
case "${tool_name}" in
  Grep | Read) ;;
  *) allow ;;
esac

cwd="$(unwin "$(json_str cwd)")"
[[ -z "${cwd}" ]] && cwd="${PWD}"
[[ -d "${cwd}" ]] || allow

# --- Activation: find a pro-coder-bootstrapped, lens-indexed project root ---

root=""
probe="${cwd}"
while [[ -n "${probe}" && "${probe}" != "/" ]]; do
  if [[ -f "${probe}/.lens/index.db" && -f "${probe}/.claude/state/pro-coder-guard" ]]; then
    root="${probe}"
    break
  fi
  parent="$(dirname "${probe}")"
  [[ "${parent}" == "${probe}" ]] && break   # hit a drive root (C:/) — stop
  probe="${parent}"
done
[[ -z "${root}" ]] && allow

# --- Denial plumbing -------------------------------------------------------

json_escape() {
  # Pure parameter expansion for the same portability reason as unwin().
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/ }"
  s="${s//$'\r'/ }"
  s="${s//$'\t'/ }"
  printf '%s' "${s}"
}

deny() {
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' \
    "$(json_escape "$1")"
  exit 0
}

# --- Rule 1: Grep ----------------------------------------------------------

if [[ "${tool_name}" == "Grep" ]]; then
  grep_path="$(unwin "$(json_str path)")"
  # A Grep aimed at one existing file is the protocol's sanctioned use: a regex
  # `lens search` cannot express, inside a file lens already pointed at.
  if [[ -n "${grep_path}" && -f "${grep_path}" ]]; then
    allow
  fi
  pattern="$(json_str pattern)"
  deny "pro-coder: tree-wide Grep is blocked while the lens index is live. Use: lens search '${pattern}' --budget 1500 [--scope <dir>] [--kind code|text] — it returns ranked file:line hits with the enclosing symbol, capped to a token budget, instead of every match uncapped. For a symbol specifically, use lens query / lens follow / lens refs. If you genuinely need a regex lens search cannot express, re-run Grep with path set to the single file lens pointed you at. Session escape hatch: PRO_CODER_GUARD=0."
fi

# --- Rule 2: Read of a large file with no range ----------------------------

file_path="$(unwin "$(json_str file_path)")"
[[ -z "${file_path}" ]] && allow
[[ -f "${file_path}" ]] || allow

# An explicit range is exactly the behaviour the guard wants to encourage.
json_has_num offset && allow
json_has_num limit && allow

# Ledger and contract files the protocol mandates reading end-to-end. Reading
# half of current-tasks.md is worse than reading all of it.
case "${file_path}" in
  */.claude/state/* | */CLAUDE.md | */current-tasks.md | */schema.txt)
    allow ;;
esac

max_lines="${LENS_GUARD_MAX_READ_LINES:-400}"
lines="$(wc -l < "${file_path}" 2>/dev/null | tr -d ' ')"
[[ -z "${lines}" ]] && allow
case "${lines}" in
  *[!0-9]*) allow ;;
esac
(( lines <= max_lines )) && allow

rel="${file_path#"${root}"/}"
deny "pro-coder: Read of ${rel} (${lines} lines) without a range is blocked while the lens index is live. Pull the slice instead: lens follow <symbol> --budget 1500, or lens slice ${rel}:<line> --budget 1500, or lens explain <symbol>. If you already know the range and are about to edit, re-run Read with offset/limit around the file:line lens anchored. Threshold is ${max_lines} lines (LENS_GUARD_MAX_READ_LINES). Session escape hatch: PRO_CODER_GUARD=0."
