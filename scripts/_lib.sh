#!/usr/bin/env bash
# Shared helpers for scripts/install.sh and scripts/uninstall.sh.
#
# Sourced — do not run directly. Compatible with bash 3.2 (macOS default) and
# bash 4+/5+ (Linux). No associative arrays, no case modifiers.
#
# Public surface:
#   sc_set_default_home              # ensures HOME is set under `set -u`
#   sc_assert_safe_dest <path> <prefix>
#                                    # exits non-zero with a stderr message if
#                                    # <path> is unsafe to write under (HOME, /,
#                                    # any system path). <prefix> is used in
#                                    # error messages (e.g. "install.sh").
#
# Single source of truth for the unsafe-dest list. install.sh and uninstall.sh
# previously carried duplicated case statements at install.sh:73-83 and
# uninstall.sh:48-58 — drift risk; consolidated here.

# Guard against repeat sourcing — idempotent if a future caller sources twice.
if [[ "${SC_LIB_LOADED:-0}" == 1 ]]; then
  return 0 2>/dev/null || exit 0
fi
SC_LIB_LOADED=1

sc_set_default_home() {
  # Default HOME so `set -u` does not trip when HOME is unset; the unsafe-dest
  # guard then refuses the empty default.
  : "${HOME:=}"
}

sc_assert_safe_dest() {
  # sc_assert_safe_dest <path> <script_prefix>
  local path="${1:-}"
  local prefix="${2:-script}"

  case "${path}" in
    "" | "/" )
      echo "${prefix}: refusing to operate on '${path}' (looks unsafe)" >&2
      return 1 ;;
    "${HOME}" | "${HOME}/" )
      echo "${prefix}: refusing to operate on HOME directly ('${path}')" >&2
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
      echo "${prefix}: refusing to operate on system path '${path}'" >&2
      return 1 ;;
  esac
  return 0
}
