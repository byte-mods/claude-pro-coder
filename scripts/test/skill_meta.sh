#!/usr/bin/env bash
# Meta-tests for pro-coder/SKILL.md — catches doc rot before users hit it.
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
skill="${repo_root}/pro-coder/SKILL.md"
skill_dir="${repo_root}/pro-coder"

# v8 split the skill into a core SKILL.md plus on-demand references/. Checks that
# are about *content existing anywhere in the protocol* scan the whole bundle;
# checks that are about the core file staying small and navigable scan only
# SKILL.md. Keeping the distinction explicit is the point — the regression this
# guards against is detail creeping back into the core until the middle sags
# again, which is exactly how the lens mandate stopped being followed.
bundle=("${skill}")
for _ref in "${skill_dir}"/references/*.md; do
  [[ -f "${_ref}" ]] && bundle+=( "${_ref}" )
done

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

# `name` value must equal "pro-coder" — the install pipeline expects it.
name_value="$(awk '/^name:[[:space:]]+/ {sub(/^name:[[:space:]]+/, ""); print; exit}' "${skill}")"
if [[ "${name_value}" == "pro-coder" ]]; then
  pass frontmatter_name_is_pro_coder
else
  fail "frontmatter_name_is_pro_coder — got '${name_value}'"
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
' "${bundle[@]}" | sort -u | awk '
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

for f in "${bundle[@]}"; do
  name="$(basename "${f}")"
  fence_count="$(grep -cE '^```' "${f}" || true)"
  if (( fence_count % 2 == 0 )); then
    pass "code_fences_balanced: ${name} (count=${fence_count})"
  else
    fail "code_fences_balanced: ${name} — odd count: ${fence_count}"
  fi
done

# --- 5. No placeholder leaks ---------------------------------------------

echo "[5] no placeholder markers"

# These should never ship in a "feature-complete" SKILL.md. Whitelist nothing.
placeholders="$(grep -nE '\<(TODO|FIXME|TBD|XXX)\>' "${bundle[@]}" | grep -vE '<(TODO|FIXME|TBD|XXX)>' || true)"
if [[ -z "${placeholders}" ]]; then
  pass no_placeholder_markers
else
  fail "no_placeholder_markers — found:
${placeholders}"
fi

# --- 6. v6+ contract: no retired fallback-mode language ------------------

echo "[6] no fallback-mode language in the lens-required triad"

# SKILL.md, README.md and scripts/install.sh are bound by a three-way contract:
# lens is required, there is no Read/Grep/Glob fallback mode. Any sentence
# that reintroduces the retired v5 behaviour in the present tense is a
# regression. Historical mentions must be past-tense and explicitly labelled
# as retired ("v5 had", "v6 removed", "retired fallback") — those are allowed.
for f in "${skill}" "${repo_root}/README.md" "${repo_root}/scripts/install.sh"; do
  name="$(basename "${f}")"
  offending="$(grep -niE 'falls? back to (Read|Grep|Glob)|fallback mode|drops? to fallback|uses? fallback' "${f}" \
    | grep -viE 'v5|removed|retired|no fallback|not "fallback|not fallback' || true)"
  if [[ -z "${offending}" ]]; then
    pass "no_fallback_language: ${name}"
  else
    fail "no_fallback_language: ${name} — found:
${offending}"
  fi
done

# --- 7. v7 protocol surface: lens verbs and modes referenced --------------

echo "[7] v7 protocol surface"

for needle in 'lens search' 'lens deps' 'lens describe' 'lens meter --diff' '## One-shot build mode' 'Token discipline' '_tokens: ~'; do
  if grep -qF -- "${needle}" "${skill}"; then
    pass "skill_mentions: ${needle}"
  else
    fail "skill_mentions: ${needle}"
  fi
done

# --- 8. v8 structure: split core, bootstrap script, enforcement hook ------
#
# Every check here guards one half of the v8 fix. The skill used to be a single
# 844-line / 76KB file in which the lens mandate sat in the sagging middle with
# nothing enforcing it; the agent followed the phase headings and silently
# skipped lens. Detail creeping back into SKILL.md, a references file drifting
# out of the index, or the hook/bootstrap going missing would each restore that
# failure mode without any other test noticing.

echo "[8] v8 structure"

core_line_budget=400
core_lines="$(wc -l < "${skill}" | tr -d ' ')"
if (( core_lines <= core_line_budget )); then
  pass "core_within_line_budget (${core_lines} <= ${core_line_budget})"
else
  fail "core_within_line_budget — SKILL.md is ${core_lines} lines, budget ${core_line_budget}. Move detail into references/."
fi

# The bootstrap script and the guard hook are the two mechanisms. Both must
# exist and both must parse — a hook with a syntax error fails open silently,
# which looks exactly like no hook at all.
for helper in "scripts/bootstrap.sh" "hooks/lens_guard.sh"; do
  if [[ -f "${skill_dir}/${helper}" ]]; then
    pass "helper_present: ${helper}"
    if bash -n "${skill_dir}/${helper}" 2>/dev/null; then
      pass "helper_parses: ${helper}"
    else
      fail "helper_parses: ${helper} — bash -n failed"
    fi
  else
    fail "helper_present: ${helper}"
    fail "helper_parses: ${helper} — file missing"
  fi
done

# The reference index in SKILL.md and the files on disk must agree in both
# directions: a pointer to a missing file sends the agent nowhere, and a file no
# pointer mentions is a file that never gets loaded.
for ref in "${skill_dir}"/references/*.md; do
  [[ -f "${ref}" ]] || continue
  rname="$(basename "${ref}")"
  if grep -qF "references/${rname}" "${skill}"; then
    pass "reference_indexed: ${rname}"
  else
    fail "reference_indexed: ${rname} — no pointer to it in SKILL.md"
  fi
done

for pointer in $(grep -oE 'references/[a-z-]+\.md' "${skill}" | sort -u); do
  if [[ -f "${skill_dir}/${pointer}" ]]; then
    pass "reference_resolves: ${pointer}"
  else
    fail "reference_resolves: ${pointer} — SKILL.md points at a file that does not exist"
  fi
done

# The two instructions that make lens as cheap as Grep. Without the ToolSearch
# line the MCP verbs stay unloaded and unusable; without the bootstrap line the
# five-step bootstrap goes back to being half-performed.
for needle in 'scripts/bootstrap.sh' 'ToolSearch' 'mcp__lens__lens_query' 'lens_guard.sh' 'PRO_CODER_GUARD'; do
  if grep -qF -- "${needle}" "${skill}"; then
    pass "core_mentions: ${needle}"
  else
    fail "core_mentions: ${needle}"
  fi
done

# --- Summary -------------------------------------------------------------

echo
echo "----------------------------------------"
echo "skill_meta: Total: ${total}, Failures: ${failures}"
echo "----------------------------------------"
[[ "${failures}" == 0 ]]
