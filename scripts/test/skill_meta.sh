#!/usr/bin/env bash
# Meta-tests for super-coder/SKILL.md — catches doc rot before users hit it.
#
# What this validates:
#   1. Frontmatter — `---` opener, `name:` and `description:` keys, `---` closer.
#   2. Required top-level sections present (Identity, Bootstrap, The Loop, etc.).
#   3. Every P-reference in the body (P1, P2, ..., P4.5, P5, P6) resolves to a
#      defined `### P<N>` header.
#   4. Markdown code-fence balance — every ``` opens or closes evenly.
#   5. No placeholder leak — `<TODO>`, `<FIXME>`, `<TBD>` should never ship.
#
# Why bash and not Python: the install pipeline already shells. Meta-tests
# shouldn't add a Python dep just for grep + counting. If checks ever need a
# real parser, switch then.
#
# Self-contained: bash 3.2+. Exits 0 on PASS, non-zero with a numeric failure
# count otherwise.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
skill="${repo_root}/super-coder/SKILL.md"

failures=0
total=0

pass() { echo "  PASS: $1"; total=$((total + 1)); }
fail() { echo "  FAIL: $1" >&2; total=$((total + 1)); failures=$((failures + 1)); }

if [[ ! -f "${skill}" ]]; then
  echo "skill_meta.sh: ${skill} not found." >&2
  exit 1
fi

# --- 1. Frontmatter ------------------------------------------------------

echo "[1] frontmatter"

frontmatter_open="$(sed -n '1p' "${skill}")"
if [[ "${frontmatter_open}" == "---" ]]; then
  pass frontmatter_opens_with_dashes
else
  fail "frontmatter_opens_with_dashes — got '${frontmatter_open}'"
fi

# Find the closing `---` within the first 10 lines.
frontmatter_close_line="$(awk 'NR>1 && /^---$/ {print NR; exit}' "${skill}")"
if [[ -n "${frontmatter_close_line}" ]] && [[ "${frontmatter_close_line}" -le 10 ]]; then
  pass frontmatter_closes_with_dashes
else
  fail "frontmatter_closes_with_dashes — no closing '---' within first 10 lines"
fi

# Required keys.
if grep -qE '^name:[[:space:]]+' "${skill}"; then
  pass frontmatter_has_name
else
  fail frontmatter_has_name
fi

if grep -qE '^description:[[:space:]]+' "${skill}"; then
  pass frontmatter_has_description
else
  fail frontmatter_has_description
fi

# `name` value must equal "super-coder" — the install pipeline expects it.
name_value="$(awk '/^name:[[:space:]]+/ {sub(/^name:[[:space:]]+/, ""); print; exit}' "${skill}")"
if [[ "${name_value}" == "super-coder" ]]; then
  pass frontmatter_name_is_super_coder
else
  fail "frontmatter_name_is_super_coder — got '${name_value}'"
fi

# --- 2. Required top-level sections --------------------------------------

echo "[2] required sections"

required_sections=(
  "## Identity"
  "## Bootstrap"
  "## The Loop"
  "## Output for the user"
  "## Resume protocol"
  "## Fast path"
  "## Hard rules"
  "## Pre-response checklist"
  "## Mid-task re-anchor"
  "## Memory"
  "## Tone"
  "## Drift anchors"
)

for header in "${required_sections[@]}"; do
  # Match the header at line start, optionally followed by ` *(... )*` decoration.
  if grep -qE "^${header}( |$| \*)" "${skill}"; then
    pass "section_present: ${header}"
  else
    fail "section_present: ${header}"
  fi
done

# --- 3. P-reference resolution -------------------------------------------

echo "[3] phase references resolve"

# Phases the loop is built on. Every one of these must appear as `### P<N>`.
required_phases=(P1 P2 P3 P4 P4.5 P5 P6)

for p in "${required_phases[@]}"; do
  # `### P1 — ...` or `### P4.5 — ...` style headers. Allow em-dash, en-dash, or space.
  if grep -qE "^### ${p}( | —| –|\$)" "${skill}"; then
    pass "phase_defined: ${p}"
  else
    fail "phase_defined: ${p}"
  fi
done

# Find any P-reference cited in body text and verify it's one of the required
# phases. Anything else (P7, P0, P-something-else) is a typo or stale ref.
# Scan only outside code fences to avoid false positives from example output.
unknown_refs="$(awk '
  /^```/ { in_code = !in_code; next }
  in_code { next }
  {
    while (match($0, /\<P[0-9](\.[0-9]+)?\>/)) {
      ref = substr($0, RSTART, RLENGTH)
      print ref
      $0 = substr($0, RSTART + RLENGTH)
    }
  }
' "${skill}" | sort -u | awk '
  BEGIN {
    valid["P1"]=1; valid["P2"]=1; valid["P3"]=1; valid["P4"]=1;
    valid["P4.5"]=1; valid["P5"]=1; valid["P6"]=1;
  }
  { if (!valid[$0]) print $0 }
')"

if [[ -z "${unknown_refs}" ]]; then
  pass no_unknown_phase_references
else
  fail "no_unknown_phase_references — found: $(echo "${unknown_refs}" | tr '\n' ' ')"
fi

# --- 4. Code-fence balance -----------------------------------------------

echo "[4] code-fence balance"

fence_count="$(grep -cE '^```' "${skill}" || true)"
if (( fence_count % 2 == 0 )); then
  pass "code_fences_balanced (count=${fence_count})"
else
  fail "code_fences_balanced — odd count: ${fence_count}"
fi

# --- 5. No placeholder leaks ---------------------------------------------

echo "[5] no placeholder markers"

# These should never ship in a "feature-complete" SKILL.md. Whitelist nothing.
placeholders="$(grep -nE '\<(TODO|FIXME|TBD|XXX)\>' "${skill}" | grep -vE '<(TODO|FIXME|TBD|XXX)>' || true)"
if [[ -z "${placeholders}" ]]; then
  pass no_placeholder_markers
else
  fail "no_placeholder_markers — found:
${placeholders}"
fi

# --- Summary -------------------------------------------------------------

echo
echo "----------------------------------------"
echo "skill_meta: Total: ${total}, Failures: ${failures}"
echo "----------------------------------------"
[[ "${failures}" == 0 ]]
