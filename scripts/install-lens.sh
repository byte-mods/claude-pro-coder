#!/usr/bin/env bash
# Build the vendored `lens` CLI and install the binary under ~/.claude/bin.
#
# Lens is pro-coder's symbol-aware code-map tool. It replaces the legacy
# `graphify` calls that P1 (lens query / lens follow) and P5 (lens . --update)
# rely on. If lens is missing at runtime, SKILL.md falls back to Read/Grep/Glob
# — degraded but functional. This script's job is to make the fast path work
# without making the slow path fail.
#
# Usage:
#   scripts/install-lens.sh                  # build + install (default)
#   scripts/install-lens.sh --bin-dir DIR    # custom binary dest (default: ~/.claude/bin)
#   scripts/install-lens.sh --force          # rebuild + reinstall even if up-to-date
#   scripts/install-lens.sh --quiet          # suppress non-error output
#   scripts/install-lens.sh --skip-if-no-cargo
#                                            # exit 0 with a warning if cargo absent
#                                            # (the default — install.sh wants this).
#                                            # Pass --require-cargo to fail loudly instead.
#   scripts/install-lens.sh --require-cargo  # exit non-zero if cargo absent
#   scripts/install-lens.sh --strict         # extra paranoia — refuse paths outside ~/.claude/
#   scripts/install-lens.sh --allow-root     # opt in to running as root (default: refuse)
#
# Every value-taking flag accepts both `--flag VALUE` and `--flag=VALUE` forms.
#
# Idempotent: re-running after a successful install is a no-op when the source
# tree is unchanged (compared via a content hash of `lens/`).

set -euo pipefail

_sc_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./_lib.sh
. "${_sc_script_dir}/_lib.sh"

sc_set_default_home

bin_dir="${HOME}/.claude/bin"
force=0
quiet=0
require_cargo=0
strict=0
allow_root=0

require_value() {
  if [[ -z "${2:-}" ]] || [[ "${2:0:2}" == "--" ]]; then
    echo "install-lens.sh: $1 requires a value" >&2
    exit 2
  fi
}

require_eq_value() {
  if [[ -z "${2:-}" ]]; then
    echo "install-lens.sh: $1 requires a value" >&2
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
    --bin-dir)         require_value "--bin-dir" "${2:-}"; bin_dir="$2"; shift 2 ;;
    --bin-dir=*)       require_eq_value "--bin-dir=" "${1#--bin-dir=}"; bin_dir="${1#--bin-dir=}"; shift ;;
    --force)           force=1; shift ;;
    --quiet)           quiet=1; shift ;;
    --skip-if-no-cargo) require_cargo=0; shift ;;
    --require-cargo)   require_cargo=1; shift ;;
    --strict)          strict=1; shift ;;
    --allow-root)      allow_root=1; shift ;;
    -h|--help)
      sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "install-lens.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

sc_assert_not_root "${EUID}" "${allow_root}" "install-lens.sh" || exit 1

repo_root="$(cd "${_sc_script_dir}/.." && pwd)"
src="${repo_root}/lens"

if [[ ! -f "${src}/Cargo.toml" ]]; then
  echo "install-lens.sh: cannot find lens crate at ${src}/Cargo.toml" >&2
  echo "install-lens.sh: this script must live at <repo>/scripts/install-lens.sh and lens/ must be vendored alongside pro-coder/" >&2
  exit 1
fi

# Refuse to write into HOME directly, /, or system paths. ~/.claude/bin is the
# default and passes the guard. --bin-dir overrides go through the same gate.
sc_assert_safe_dest "${bin_dir}" "install-lens.sh" || exit 1

if [[ "${strict}" == 1 ]]; then
  sc_assert_strict_allowed "${bin_dir}" "${HOME}" "install-lens.sh" || exit 1
fi

bin_path="${bin_dir}/lens"

# Cargo presence is the gating prerequisite. Without it we cannot build.
if ! command -v cargo >/dev/null 2>&1; then
  msg="install-lens.sh: cargo not found on PATH. Install Rust toolchain (https://rustup.rs) and re-run."
  if [[ "${require_cargo}" == 1 ]]; then
    echo "${msg}" >&2
    exit 1
  fi
  echo "${msg}" >&2
  echo "install-lens.sh: skipping lens build. pro-coder will fall back to Read/Grep/Glob at runtime." >&2
  exit 0
fi

# Hash the source tree so re-runs without source changes are no-ops. We hash
# every tracked-relevant file (Cargo.{toml,lock} + crates/) so that a `git pull`
# bump on the vendor tree triggers a rebuild while a touched .DS_Store does not.
compute_src_hash() {
  # `find -print0 | sort -z | xargs -0 cat | shasum` is portable across macOS
  # and Linux. Avoids GNU-only flags. Output: 40-char sha1 only.
  (cd "${src}" && \
    find Cargo.toml Cargo.lock crates -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.sql' -o -name 'Cargo.lock' \) -print0 \
    | LC_ALL=C sort -z \
    | xargs -0 cat \
    | shasum \
    | awk '{print $1}'
  )
}

src_hash="$(compute_src_hash)"
hash_marker="${bin_dir}/.lens.installed.sha"

# Idempotency: if the binary exists, reports the expected version, and the
# source hash matches the marker, do nothing.
if [[ "${force}" != 1 ]] && [[ -x "${bin_path}" ]] && [[ -f "${hash_marker}" ]]; then
  if [[ "$(cat "${hash_marker}")" == "${src_hash}" ]]; then
    if "${bin_path}" --version >/dev/null 2>&1; then
      log "install-lens.sh: ${bin_path} already up-to-date (src hash matches). No action."
      exit 0
    fi
  fi
fi

log "install-lens.sh: building lens (release profile) from ${src}"

# Build in the vendor tree. Output goes under lens/target/ — gitignored so it
# never leaks into the repo. Surface stderr so a failed build is visible.
build_log="$(mktemp -t lens-build.XXXXXX)"
trap 'rm -f "${build_log}"' EXIT

if [[ "${quiet}" == 1 ]]; then
  if ! (cd "${src}" && cargo build --release --quiet) >"${build_log}" 2>&1; then
    echo "install-lens.sh: cargo build failed. Last 40 lines of build log:" >&2
    tail -n 40 "${build_log}" >&2
    exit 1
  fi
else
  if ! (cd "${src}" && cargo build --release); then
    echo "install-lens.sh: cargo build failed. See output above." >&2
    exit 1
  fi
fi

built="${src}/target/release/lens"
if [[ ! -x "${built}" ]]; then
  echo "install-lens.sh: build succeeded but ${built} is missing or not executable." >&2
  exit 1
fi

mkdir -p "${bin_dir}"

# Atomic install: copy to a sibling tmp file in the same dir, chmod, then mv.
# mv on the same filesystem is atomic — closes the TOCTOU window between cp
# and a partial-read by another shell.
staging="$(mktemp "${bin_dir}/.lens.staging.XXXXXX")"
trap 'rm -f "${staging}" "${build_log}"' EXIT
cp "${built}" "${staging}"
chmod 0755 "${staging}"
mv "${staging}" "${bin_path}"
echo "${src_hash}" > "${hash_marker}"
trap 'rm -f "${build_log}"' EXIT

# Verify by invoking --version. If the binary doesn't run, the install isn't
# real — surface that loudly rather than letting a broken artefact sit on PATH.
if ! installed_version="$("${bin_path}" --version 2>/dev/null)"; then
  echo "install-lens.sh: ${bin_path} did not respond to --version. Investigate." >&2
  exit 1
fi

log "install-lens.sh: installed ${installed_version} -> ${bin_path}"

# PATH note. Skip if bin_dir is already on PATH; otherwise tell the user how
# to add it. Match against ":${PATH}:" so we don't false-match a substring.
case ":${PATH}:" in
  *":${bin_dir}:"*) ;;
  *)
    log "install-lens.sh: NOTE — ${bin_dir} is not on your PATH."
    log "install-lens.sh: add this to your shell rc (~/.zshrc or ~/.bashrc):"
    log "install-lens.sh:     export PATH=\"${bin_dir}:\$PATH\""
    ;;
esac
