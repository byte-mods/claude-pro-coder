#!/usr/bin/env bash
# Shared helpers for scripts/install.sh and scripts/uninstall.sh.
#
# Sourced — do not run directly. Compatible with bash 3.2 (macOS default) and
# bash 4+/5+ (Linux). No associative arrays, no case modifiers.
#
#   sc_version                       # emits the project version (reads VERSION
#                                    # at repo root) on stdout.
#
# Public surface:
#   sc_set_default_home              # ensures HOME is set under `set -u`
#   sc_canonicalize_dest <path>      # emits an absolute, '.'-and-'..'-resolved
#                                    # path on stdout. Pure bash; no realpath
#                                    # dep. Path need not exist.
#   sc_assert_safe_dest <path> <prefix>
#                                    # canonicalizes <path>, then exits
#                                    # non-zero with a stderr message if it
#                                    # resolves to an unsafe location (HOME,
#                                    # /, any system path). <prefix> is used
#                                    # in error messages (e.g. "install.sh").
#   sc_assert_not_root <euid> <allow_root_flag> <prefix>
#                                    # refuses to proceed when EUID=0 unless
#                                    # allow_root=1. Dep-injected EUID so
#                                    # tests can fake it. Warns even on
#                                    # allow_root=1 because root-owned files
#                                    # under $HOME are a future-self footgun.
#   sc_assert_strict_allowed <path> <home> <prefix>
#                                    # positive allow-list — only permits
#                                    # paths under ${home}/.claude/. Used
#                                    # when --strict is set. Layered on top
#                                    # of sc_assert_safe_dest, not replacing.
#
# Single source of truth for the unsafe-dest list. install.sh and uninstall.sh
# previously carried duplicated case statements at install.sh:73-83 and
# uninstall.sh:48-58 — drift risk; consolidated here.
#
# Canonicalisation closes a `..`-traversal bypass: a caller passing
# `--dest /Users/me/skills/../../../etc` previously slipped past the guard
# because the literal didn't case-match `/etc/*`. Canonicalising first means
# the resolved path (`/etc`) is what the case-match sees.

# Guard against repeat sourcing — idempotent if a future caller sources twice.
if [[ "${SC_LIB_LOADED:-0}" == 1 ]]; then
  return 0 2>/dev/null || exit 0
fi
SC_LIB_LOADED=1

sc_version() {
  # Emit the project version from the VERSION file at the repo root. _lib.sh
  # lives at scripts/_lib.sh, so the repo root is one directory up from here.
  local lib_dir repo_root
  lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  repo_root="$(cd "${lib_dir}/.." && pwd)"
  if [[ -f "${repo_root}/VERSION" ]]; then
    cat "${repo_root}/VERSION"
  else
    echo "unknown"
  fi
}

sc_set_default_home() {
  # Default HOME so `set -u` does not trip when HOME is unset; the unsafe-dest
  # guard then refuses the empty default.
  : "${HOME:=}"
}

sc_canonicalize_dest() {
  # Emit an absolute, '.'-and-'..'-resolved path on stdout. Bash-3.2 compatible
  # (macOS default). The path need not exist — we never call cd or stat. Pure
  # textual normalisation, identical in spirit to `realpath -m`.
  #
  # Algorithm (O(n) in path length):
  #   1. If relative, prepend $PWD.
  #   2. Split on '/' into segments.
  #   3. Drop empty segments (collapse '//') and '.' segments.
  #   4. For each '..', pop the previous segment (clamps at root, like realpath).
  #   5. Reassemble.
  local path="${1:-}"

  # Empty -> empty; caller's safety guard rejects it.
  if [[ -z "${path}" ]]; then
    echo ""
    return 0
  fi

  # Make absolute by prepending PWD if not already.
  case "${path}" in
    /*) ;;
    *)  path="${PWD%/}/${path}" ;;
  esac

  # Split into segments without relying on $IFS (which can leak to callers).
  local rest="${path#/}"  # strip leading '/' so the loop is uniform
  local parts=()
  while [[ "${rest}" == *"/"* ]]; do
    parts+=( "${rest%%/*}" )
    rest="${rest#*/}"
  done
  parts+=( "${rest}" )

  # Resolve segments. Always pop the last index — never leaves array holes.
  local out=()
  local seg
  if [[ ${#parts[@]} -gt 0 ]]; then
    for seg in "${parts[@]}"; do
      case "${seg}" in
        "" | ".") ;;  # skip empty (from '//') and current-dir
        "..")
          if [[ ${#out[@]} -gt 0 ]]; then
            unset 'out[${#out[@]}-1]'
          fi
          ;;
        *) out+=( "${seg}" ) ;;
      esac
    done
  fi

  if [[ ${#out[@]} -eq 0 ]]; then
    echo "/"
    return 0
  fi
  local result=""
  for seg in "${out[@]}"; do
    result+="/${seg}"
  done
  echo "${result}"
}

sc_assert_not_root() {
  # sc_assert_not_root <euid> <allow_root_flag> <script_prefix>
  #
  # Refuses to proceed when the effective UID is 0 (root) unless the caller
  # explicitly opted in via --allow-root (passed as 1 here). EUID is dependency-
  # injected so tests can exercise both paths without actually being root.
  #
  # Reason: every path the install scripts touch (~/.claude/, ~/.claude.json) is
  # per-user state. Running as root creates files owned by root that the user's
  # normal shell cannot edit later — a footgun that surfaces hours after install.
  local euid="${1:-}"
  local allow_root="${2:-0}"
  local prefix="${3:-script}"
  if [[ "${euid}" == "0" ]] && [[ "${allow_root}" != "1" ]]; then
    echo "${prefix}: refusing to run as root (EUID=0). The install touches per-user state under \$HOME;" >&2
    echo "${prefix}: running as root creates root-owned files that your normal shell cannot edit later." >&2
    echo "${prefix}: re-run as your normal user, OR pass --allow-root if you really mean to (e.g. inside a CI container)." >&2
    return 1
  fi
  if [[ "${euid}" == "0" ]] && [[ "${allow_root}" == "1" ]]; then
    echo "${prefix}: WARNING — running as root with --allow-root. Files under \$HOME will be root-owned." >&2
  fi
  return 0
}

sc_assert_strict_allowed() {
  # sc_assert_strict_allowed <path> <home> <script_prefix>
  #
  # Positive allow-list — used when --strict is set. Refuses any path that does
  # not resolve to a location under ${home}/.claude/. Layered on top of the
  # negative unsafe-dest guard, not replacing it: --strict callers run BOTH
  # checks. Useful when the user wants extra paranoia (e.g. paths smuggled in
  # via --dest from a config file).
  local raw="${1:-}"
  local home="${2:-${HOME:-}}"
  local prefix="${3:-script}"
  local path
  path="$(sc_canonicalize_dest "${raw}")"

  if [[ -z "${home}" ]]; then
    echo "${prefix}: --strict requires HOME to be set; cannot validate." >&2
    return 1
  fi

  case "${path}" in
    "${home}/.claude" | "${home}/.claude/"* )
      return 0 ;;
    * )
      echo "${prefix}: --strict mode: refusing path '${raw}' (resolved to '${path}'); only paths under '${home}/.claude/' are allowed." >&2
      return 1 ;;
  esac
}

sc_assert_safe_dest() {
  # sc_assert_safe_dest <path> <script_prefix>
  #
  # Canonicalises <path> first, then case-matches against the unsafe-dest list.
  # Error messages cite both the raw input and the resolved form so a user who
  # passed a `..`-traversal sees what was actually rejected.
  local raw="${1:-}"
  local prefix="${2:-script}"
  local path
  path="$(sc_canonicalize_dest "${raw}")"

  case "${path}" in
    "" | "/" )
      echo "${prefix}: refusing to operate on '${raw}' (resolves to '${path}', looks unsafe)" >&2
      return 1 ;;
    "${HOME}" | "${HOME}/" )
      echo "${prefix}: refusing to operate on HOME directly ('${raw}' -> '${path}')" >&2
      return 1 ;;
    # System paths — refuse the path itself AND anything under it.
    # Cross-platform: macOS-specific (/Applications /Network /Volumes /private /System /Library)
    # and Linux-specific (/home /root /srv /run /lib /lib64 /mnt /media /boot /proc /sys /dev /etc /var /usr /opt /bin /sbin)
    # are both included so the same script protects users on either OS.
    /bin | /bin/* | /sbin | /sbin/* \
    | /usr | /usr/* | /etc | /etc/* | /var | /var/* \
    | /System | /System/* | /Library | /Library/* \
    | /opt | /opt/* | /boot | /boot/* \
    | /dev | /dev/* | /proc | /proc/* | /sys | /sys/* \
    | /Applications | /Applications/* | /Network | /Network/* \
    | /Volumes | /Volumes/* | /private | /private/* \
    | /home | /home/* | /root | /root/* | /srv | /srv/* \
    | /run | /run/* | /lib | /lib/* | /lib64 | /lib64/* \
    | /mnt | /mnt/* | /media | /media/* )
      echo "${prefix}: refusing to operate on system path '${raw}' (resolved to '${path}')" >&2
      return 1 ;;
  esac
  return 0
}
