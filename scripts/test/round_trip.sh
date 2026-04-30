#!/usr/bin/env bash
# Integration tests for the install.sh / uninstall.sh round-trip and the
# shared safety helpers in _lib.sh.
#
# Self-contained: no external test framework, just bash 3.2+ and the scripts
# under test. Exits 0 on PASS, non-zero with a numeric failure count otherwise.
#
# Sub-tests:
#   1. test_canonicalize          — sc_canonicalize_dest in 13 textual cases
#   2. test_safe_dest_guard       — sc_assert_safe_dest accepts/rejects correctly
#   3. test_orphan_reap           — uninstall.sh reaps .pro-coder.staging.*
#   4. test_round_trip_copy       — install --copy → assert → uninstall → assert
#   5. test_round_trip_symlink    — install --symlink → assert → uninstall → assert
#   6. test_install_extended_flags — --dest=VALUE form, --quiet, --dry-run on install.sh
#   7. test_skill_meta            — frontmatter, required sections, P-refs, code-fence balance
#   8. test_strict_and_root_guards — sc_assert_strict_allowed and sc_assert_not_root
#
# Why /tmp not ${TMPDIR}: macOS sets TMPDIR to /var/folders/... which the
# safety guard correctly refuses (matches /var/*). Tests use /tmp directly;
# on macOS that's a symlink to /private/tmp but canonicalisation is textual,
# so /tmp/foo stays /tmp/foo and is not rejected by /private/*.
#
# Tests use --no-lens --no-mcp on install (no cargo build in unit tests) and
# --keep-lens --keep-mcp on uninstall (lens/mcp were never installed; this
# avoids needing to set up a fake binary just to satisfy the cleanup path).

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
scripts_dir="$(cd "${script_dir}/.." && pwd)"
repo_root="$(cd "${scripts_dir}/.." && pwd)"

# shellcheck source=../_lib.sh
. "${scripts_dir}/_lib.sh"
sc_set_default_home

# --- Test harness ---------------------------------------------------------

failures=0
total=0

# One parent jail under /tmp — every sub-test creates a child dir inside it.
# A single rm -rf at exit cleans the whole tree. Avoids the subshell-array
# bug that an array-tracked approach would hit (make_jail invoked via
# `jail="$(make_jail)"` runs in a subshell whose appends to JAILS would not
# propagate back to the parent — leaking /tmp dirs on every run).
PARENT_JAIL="$(mktemp -d "/tmp/sc-test.XXXXXX")"

cleanup_parent_jail() {
  if [[ -n "${PARENT_JAIL}" && -d "${PARENT_JAIL}" ]]; then
    rm -rf "${PARENT_JAIL}"
  fi
}
trap cleanup_parent_jail EXIT

make_jail() {
  # Child dir under PARENT_JAIL. Echoed for capture via "$(make_jail)".
  # No array bookkeeping needed — cleanup_parent_jail reaps everything in one
  # rm at exit, regardless of whether we ran inside a subshell.
  mktemp -d "${PARENT_JAIL}/jail.XXXXXX"
}

pass() { echo "  PASS: $1"; total=$((total + 1)); }
fail() { echo "  FAIL: $1" >&2; total=$((total + 1)); failures=$((failures + 1)); }

assert_eq() {
  # assert_eq <name> <actual> <expected>
  if [[ "$2" == "$3" ]]; then
    pass "$1"
  else
    fail "$1 — got='$2' want='$3'"
  fi
}

assert_true() {
  # assert_true <name> <command...>
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    pass "${name}"
  else
    fail "${name} — command failed: $*"
  fi
}

assert_false() {
  # assert_false <name> <command...>
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    fail "${name} — command unexpectedly succeeded: $*"
  else
    pass "${name}"
  fi
}

# --- Test 1: canonicalisation --------------------------------------------

test_canonicalize() {
  echo "[1] test_canonicalize"
  assert_eq absolute_passthrough     "$(sc_canonicalize_dest /Users/me)"           "/Users/me"
  assert_eq trailing_slash_stripped  "$(sc_canonicalize_dest /Users/me/)"          "/Users/me"
  assert_eq dotdot_bypass            "$(sc_canonicalize_dest /Users/me/../../etc)" "/etc"
  assert_eq single_dot_collapsed     "$(sc_canonicalize_dest /Users/me/./skills)"  "/Users/me/skills"
  assert_eq double_slash_collapsed   "$(sc_canonicalize_dest /Users//me//skills)"  "/Users/me/skills"
  assert_eq triple_slash             "$(sc_canonicalize_dest ///foo///bar)"        "/foo/bar"
  assert_eq empty_input              "$(sc_canonicalize_dest "")"                  ""
  assert_eq root_passthrough         "$(sc_canonicalize_dest /)"                   "/"
  assert_eq dotdot_past_root         "$(sc_canonicalize_dest /../..)"              "/"
  assert_eq just_dotdot              "$(sc_canonicalize_dest /a/b/../..)"          "/"
  # Relative-path resolution against PWD.
  local saved="${PWD}"
  cd /tmp
  assert_eq relative_against_pwd     "$(sc_canonicalize_dest foo)"                 "/tmp/foo"
  assert_eq leading_dot_relative     "$(sc_canonicalize_dest ./foo)"               "/tmp/foo"
  assert_eq leading_dotdot_relative  "$(sc_canonicalize_dest ../foo)"              "/foo"
  cd "${saved}"
}

# --- Test 2: safe-dest guard ---------------------------------------------

test_safe_dest_guard() {
  echo "[2] test_safe_dest_guard"
  HOME=/Users/qa-test
  assert_true  legit_dest_accepted   sc_assert_safe_dest "/Users/qa-test/.claude/skills" "test"
  assert_false home_itself_rejected  sc_assert_safe_dest "/Users/qa-test"                "test"
  assert_false root_rejected         sc_assert_safe_dest "/"                              "test"
  assert_false empty_rejected        sc_assert_safe_dest ""                               "test"
  assert_false etc_rejected          sc_assert_safe_dest "/etc"                           "test"
  assert_false dotdot_bypass_blocked sc_assert_safe_dest "/Users/qa-test/../../../etc"    "test"
  assert_false private_rejected      sc_assert_safe_dest "/private/var/foo"               "test"
  assert_false applications_rejected sc_assert_safe_dest "/Applications/foo"              "test"
}

# --- Test 3: orphan staging reap -----------------------------------------

test_orphan_reap() {
  echo "[3] test_orphan_reap"
  local jail; jail="$(make_jail)"
  local skills="${jail}/skills"
  mkdir -p "${skills}/.pro-coder.staging.AAA111/lib"
  mkdir -p "${skills}/.pro-coder.staging.BBB222"
  mkdir -p "${skills}/.pro-coder.staging.CCC333"
  mkdir -p "${skills}/some-other-skill"
  echo "untouched" > "${skills}/some-other-skill/file.txt"

  "${scripts_dir}/uninstall.sh" \
    --dest "${skills}" \
    --bin-dir "${jail}/bin" \
    --claude-json "${jail}/cj" \
    --keep-lens --keep-mcp --quiet >/dev/null 2>&1 || true

  assert_false orphan_AAA111_reaped  test -e "${skills}/.pro-coder.staging.AAA111"
  assert_false orphan_BBB222_reaped  test -e "${skills}/.pro-coder.staging.BBB222"
  assert_false orphan_CCC333_reaped  test -e "${skills}/.pro-coder.staging.CCC333"
  assert_true  unrelated_dir_intact  test -d "${skills}/some-other-skill"
  assert_eq    unrelated_file_intact "$(cat "${skills}/some-other-skill/file.txt")" "untouched"

  # Dry-run must NOT remove orphans.
  local jail2; jail2="$(make_jail)"
  mkdir -p "${jail2}/skills/.pro-coder.staging.DRY1"
  "${scripts_dir}/uninstall.sh" \
    --dest "${jail2}/skills" \
    --bin-dir "${jail2}/bin" \
    --claude-json "${jail2}/cj" \
    --keep-lens --keep-mcp --dry-run --quiet >/dev/null 2>&1 || true
  assert_true  dryrun_preserves_orphan test -d "${jail2}/skills/.pro-coder.staging.DRY1"

  # Relative --dest: closes the gap that absolute-only tests would miss.
  # Validates T1+T2 composed end-to-end when uninstall is invoked from inside
  # the jail with a bare relative path.
  local jail3; jail3="$(make_jail)"
  mkdir -p "${jail3}/skills/.pro-coder.staging.REL1"
  local saved_pwd="${PWD}"
  cd "${jail3}"
  "${scripts_dir}/uninstall.sh" \
    --dest "skills" \
    --bin-dir "bin" \
    --claude-json "cj" \
    --keep-lens --keep-mcp --quiet >/dev/null 2>&1 || true
  cd "${saved_pwd}"
  assert_false relative_dest_orphan_reaped \
    test -e "${jail3}/skills/.pro-coder.staging.REL1"
}

# --- Test 4 + 5: round trip ----------------------------------------------

run_round_trip() {
  local mode="$1"     # "copy" or "symlink"
  local mode_flag="--${mode}"

  local jail; jail="$(make_jail)"
  local skills="${jail}/skills"
  mkdir -p "${skills}"

  local src_skill="${repo_root}/pro-coder/SKILL.md"
  if [[ ! -f "${src_skill}" ]]; then
    fail "round_trip_${mode}_source_present"
    return
  fi
  local src_size_before
  src_size_before="$(wc -c < "${src_skill}")"

  # Install. --no-lens --no-mcp keeps the test pure (no cargo, no claude.json edit).
  if ! "${scripts_dir}/install.sh" \
        "${mode_flag}" \
        --dest "${skills}" \
        --no-lens --no-mcp \
        >"${jail}/install.log" 2>&1; then
    fail "round_trip_${mode}_install_succeeded — see ${jail}/install.log"
    return
  fi
  pass "round_trip_${mode}_install_succeeded"

  assert_true round_trip_${mode}_skill_md_present \
    test -f "${skills}/pro-coder/SKILL.md"

  if [[ "${mode}" == "symlink" ]]; then
    assert_true round_trip_symlink_dest_is_link \
      test -L "${skills}/pro-coder"
    local link_target
    link_target="$(readlink "${skills}/pro-coder")"
    assert_eq round_trip_symlink_target_correct \
      "${link_target}" "${repo_root}/pro-coder"
  else
    if [[ -d "${skills}/pro-coder" ]] && [[ ! -L "${skills}/pro-coder" ]]; then
      pass round_trip_copy_dest_is_real_dir
    else
      fail round_trip_copy_dest_is_real_dir
    fi
  fi

  # Idempotency: re-running install in the same mode must succeed (no-op).
  if ! "${scripts_dir}/install.sh" \
        "${mode_flag}" \
        --dest "${skills}" \
        --no-lens --no-mcp \
        >"${jail}/install2.log" 2>&1; then
    fail "round_trip_${mode}_install_idempotent — see ${jail}/install2.log"
  else
    pass "round_trip_${mode}_install_idempotent"
  fi

  # Uninstall. --keep-lens/--keep-mcp because they were never installed.
  if ! "${scripts_dir}/uninstall.sh" \
        --dest "${skills}" \
        --bin-dir "${jail}/bin" \
        --claude-json "${jail}/cj" \
        --keep-lens --keep-mcp --quiet \
        >"${jail}/uninstall.log" 2>&1; then
    fail "round_trip_${mode}_uninstall_succeeded — see ${jail}/uninstall.log"
    return
  fi
  pass "round_trip_${mode}_uninstall_succeeded"

  if [[ ! -e "${skills}/pro-coder" ]] && [[ ! -L "${skills}/pro-coder" ]]; then
    pass "round_trip_${mode}_dest_gone"
  else
    fail "round_trip_${mode}_dest_gone"
  fi
  assert_true round_trip_${mode}_skills_parent_intact \
    test -d "${skills}"
  assert_true round_trip_${mode}_source_skill_md_intact \
    test -f "${src_skill}"

  local src_size_after
  src_size_after="$(wc -c < "${src_skill}")"
  assert_eq round_trip_${mode}_source_size_unchanged \
    "${src_size_after}" "${src_size_before}"

  # Idempotency on uninstall: second run must be a clean no-op.
  if ! "${scripts_dir}/uninstall.sh" \
        --dest "${skills}" \
        --bin-dir "${jail}/bin" \
        --claude-json "${jail}/cj" \
        --keep-lens --keep-mcp --quiet \
        >"${jail}/uninstall2.log" 2>&1; then
    fail "round_trip_${mode}_uninstall_idempotent — see ${jail}/uninstall2.log"
  else
    pass "round_trip_${mode}_uninstall_idempotent"
  fi
}

test_round_trip_copy()    { echo "[4] test_round_trip_copy";    run_round_trip copy; }
test_round_trip_symlink() { echo "[5] test_round_trip_symlink"; run_round_trip symlink; }

# --- Test 6: extended-flag forms (=VALUE, --quiet, --dry-run) -------------

test_install_extended_flags() {
  echo "[6] test_install_extended_flags"

  # 6.1 — install.sh --dest=VALUE form (the `=` parser, not the spaced form).
  local jail; jail="$(make_jail)"
  local skills="${jail}/skills"
  mkdir -p "${skills}"
  if "${scripts_dir}/install.sh" \
        --copy \
        "--dest=${skills}" \
        --no-lens --no-mcp \
        >"${jail}/eq.log" 2>&1; then
    pass install_dest_eq_form_accepted
  else
    fail "install_dest_eq_form_accepted — see ${jail}/eq.log"
  fi
  assert_true install_dest_eq_form_landed \
    test -f "${skills}/pro-coder/SKILL.md"

  # 6.2 — uninstall.sh --dest=VALUE form (symmetric coverage).
  if "${scripts_dir}/uninstall.sh" \
        "--dest=${skills}" \
        "--bin-dir=${jail}/bin" \
        "--claude-json=${jail}/cj" \
        --keep-lens --keep-mcp --quiet \
        >"${jail}/uninstall_eq.log" 2>&1; then
    pass uninstall_dest_eq_form_accepted
  else
    fail "uninstall_dest_eq_form_accepted — see ${jail}/uninstall_eq.log"
  fi
  assert_false uninstall_dest_eq_form_removed \
    test -e "${skills}/pro-coder"

  # 6.3 — install.sh --quiet produces no stdout on success.
  local jail2; jail2="$(make_jail)"
  local skills2="${jail2}/skills"
  mkdir -p "${skills2}"
  local quiet_stdout
  quiet_stdout="$("${scripts_dir}/install.sh" \
        --copy \
        --dest "${skills2}" \
        --no-lens --no-mcp \
        --quiet 2>"${jail2}/quiet.err")"
  if [[ -z "${quiet_stdout}" ]]; then
    pass install_quiet_silences_stdout
  else
    fail "install_quiet_silences_stdout — got stdout: ${quiet_stdout}"
  fi
  # Quiet still installs.
  assert_true install_quiet_still_installs \
    test -f "${skills2}/pro-coder/SKILL.md"

  # 6.4 — install.sh --dry-run does NOT create dest.
  local jail3; jail3="$(make_jail)"
  local skills3="${jail3}/skills"
  mkdir -p "${skills3}"
  if "${scripts_dir}/install.sh" \
        --copy \
        --dest "${skills3}" \
        --no-lens --no-mcp \
        --dry-run \
        >"${jail3}/dryrun.log" 2>&1; then
    pass install_dry_run_succeeded
  else
    fail "install_dry_run_succeeded — see ${jail3}/dryrun.log"
  fi
  assert_false install_dry_run_no_dest_created \
    test -e "${skills3}/pro-coder"
  # The dry-run log must mention what *would* happen — the "would copy" line.
  if grep -q "would copy" "${jail3}/dryrun.log"; then
    pass install_dry_run_logs_intent
  else
    fail "install_dry_run_logs_intent — log lacked 'would copy' line"
  fi
  # Dry-run is idempotent (re-runnable, still no side-effect).
  if "${scripts_dir}/install.sh" \
        --copy \
        --dest "${skills3}" \
        --no-lens --no-mcp \
        --dry-run \
        >"${jail3}/dryrun2.log" 2>&1; then
    pass install_dry_run_idempotent
  else
    fail "install_dry_run_idempotent — see ${jail3}/dryrun2.log"
  fi
  assert_false install_dry_run_idempotent_no_dest \
    test -e "${skills3}/pro-coder"

  # 6.5 — empty value via `--flag=` is rejected (require_eq_value contract).
  if "${scripts_dir}/install.sh" "--dest=" --copy --no-lens --no-mcp \
        >/dev/null 2>"${jail3}/empty.err"; then
    fail "install_empty_eq_value_rejected — exited 0 unexpectedly"
  else
    pass install_empty_eq_value_rejected
  fi
}

# --- Test 7: SKILL.md meta-tests (delegated to skill_meta.sh) ------------

test_skill_meta() {
  echo "[7] test_skill_meta"
  if "${script_dir}/skill_meta.sh" >/dev/null 2>&1; then
    pass skill_meta_all_checks_passed
  else
    fail "skill_meta_all_checks_passed — run scripts/test/skill_meta.sh directly to see which check failed"
  fi
}

# --- Test 8: --strict allow-list + sc_assert_not_root --------------------

test_strict_and_root_guards() {
  echo "[8] test_strict_and_root_guards"

  # 8.1 — sc_assert_strict_allowed accepts paths under ${HOME}/.claude/.
  HOME=/Users/qa-test
  assert_true  strict_accepts_skills_dir \
    sc_assert_strict_allowed "/Users/qa-test/.claude/skills" "/Users/qa-test" "test"
  assert_true  strict_accepts_bin_dir \
    sc_assert_strict_allowed "/Users/qa-test/.claude/bin" "/Users/qa-test" "test"
  assert_true  strict_accepts_claude_root \
    sc_assert_strict_allowed "/Users/qa-test/.claude" "/Users/qa-test" "test"
  assert_false strict_rejects_other_subtree \
    sc_assert_strict_allowed "/Users/qa-test/projects/foo" "/Users/qa-test" "test"
  assert_false strict_rejects_dotdot_bypass \
    sc_assert_strict_allowed "/Users/qa-test/.claude/../projects/foo" "/Users/qa-test" "test"
  assert_false strict_rejects_tmp \
    sc_assert_strict_allowed "/tmp/foo" "/Users/qa-test" "test"

  # 8.2 — sc_assert_not_root: dependency-injected EUID lets us test both paths.
  assert_true  not_root_passes_when_euid_nonzero \
    sc_assert_not_root 1000 0 "test"
  assert_false not_root_refuses_when_euid_zero_no_optin \
    sc_assert_not_root 0 0 "test"
  assert_true  not_root_passes_when_root_with_optin \
    sc_assert_not_root 0 1 "test"

  # 8.3 — install.sh end-to-end with --strict: skills under ~/.claude/ pass,
  # outside fails. Use a fake HOME so we can keep the test on /tmp.
  local jail; jail="$(make_jail)"
  local fake_home="${jail}/home"
  mkdir -p "${fake_home}/.claude/skills" "${fake_home}/.claude/bin"

  if HOME="${fake_home}" "${scripts_dir}/install.sh" \
        --copy --strict \
        --dest "${fake_home}/.claude/skills" \
        --bin-dir "${fake_home}/.claude/bin" \
        --claude-json "${fake_home}/.claude/claude.json" \
        --no-lens --no-mcp \
        >"${jail}/strict_pass.log" 2>&1; then
    pass strict_accepts_path_under_claude_home
  else
    fail "strict_accepts_path_under_claude_home — see ${jail}/strict_pass.log"
  fi

  # --strict + dest outside ~/.claude/ must fail. Use a different parent inside
  # the jail (still safe-dest-allowed because it's under /tmp/... which is not
  # in the unsafe list).
  local outside="${jail}/elsewhere"
  mkdir -p "${outside}"
  if HOME="${fake_home}" "${scripts_dir}/install.sh" \
        --copy --strict \
        --dest "${outside}" \
        --no-lens --no-mcp \
        >"${jail}/strict_fail.log" 2>&1; then
    fail "strict_rejects_path_outside_claude_home — install.sh succeeded unexpectedly"
  else
    pass strict_rejects_path_outside_claude_home
  fi
}

# --- Driver --------------------------------------------------------------

test_canonicalize
test_safe_dest_guard
test_orphan_reap
test_round_trip_copy
test_round_trip_symlink
test_install_extended_flags
test_skill_meta
test_strict_and_root_guards

echo
echo "----------------------------------------"
echo "Total: ${total}, Failures: ${failures}"
echo "----------------------------------------"
[[ "${failures}" == 0 ]]
