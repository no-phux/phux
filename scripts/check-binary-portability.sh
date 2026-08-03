#!/usr/bin/env bash
# Assert that release binaries link only against libraries that exist on an
# ordinary host, on both macOS and Linux.
#
# Usage: scripts/check-binary-portability.sh <binary> [<binary>...]
#
# WHY THIS EXISTS: release.yml grep'd `otool -L` for "/nix/store" on macOS and
# checked NOTHING on Linux. That caught exactly one failure mode on exactly one
# platform. A Homebrew-linked dylib on macOS, or any non-baseline .so on Linux,
# shipped green and failed on the user's machine at exec time — the worst place
# to find out, because the tarball and its checksum both verify fine.
#
# The rule enforced here is: a release binary may depend only on libraries the
# target OS itself ships. Everything else is a build-host leak.
#
#   macOS   /usr/lib/** and /System/Library/** only. A Nix, Homebrew, MacPorts,
#           or /usr/local path is a leak. So is an unresolved @rpath/@loader_path
#           entry, which is a dangling dependency wearing a relative name.
#   Linux   The glibc runtime set only (libc, libm, libgcc_s, libdl, libpthread,
#           librt, libutil, ld-linux). Anything else — and any /nix/store path in
#           the resolved output — is a leak.
#
# GLIBC SYMBOL VERSIONS are also checked, because a clean NEEDED list still
# breaks on older distros: the Linux legs build on ubuntu-22.04 (glibc 2.35)
# precisely to keep this floor low, and a runner image bump would silently raise
# it. PHUX_GLIBC_MAX overrides the ceiling.
#
# NOT COVERED: CPU baseline. libghostty-vt's build.rs passes -Dtarget only when
# `target != host` (crates/libghostty-vt-sys/build.rs), so the native release
# legs let zig auto-detect host CPU features and can emit instructions the
# runner has but an older machine does not. That is an ILLEGAL INSTRUCTION at
# runtime, invisible to any link-level check including this one, and it is fixed
# in the libghostty-rs fork, not here. See release.yml's header.
set -euo pipefail

GLIBC_MAX="${PHUX_GLIBC_MAX:-2.35}"
failures=0

fail() {
  printf 'portability: %s\n' "$1" >&2
  failures=$((failures + 1))
}

# 2.35 -> 2035000 style key so `sort -V`-free numeric comparison is possible in
# pure bash (the runners have coreutils, macOS does not have `sort -V` before
# Sonoma and this must behave identically on both).
ver_key() {
  local major minor
  major="${1%%.*}"
  minor="${1#*.}"
  minor="${minor%%.*}"
  [ "$minor" = "$1" ] && minor=0
  printf '%d\n' $((major * 1000 + minor))
}

check_macho() {
  local bin="$1" line lib
  # Skip the first line (the binary's own path) and the LC_ID_DYLIB of the
  # image itself; every remaining line is a load command.
  while read -r line; do
    lib="${line%% (compatibility*}"
    lib="${lib#"${lib%%[![:space:]]*}"}"
    [ -n "$lib" ] || continue
    case "$lib" in
      "$bin"*) ;;
      /usr/lib/*|/System/Library/*) ;;
      *)
        fail "$bin links a non-system library: $lib"
        ;;
    esac
  done < <(otool -L "$bin" | tail -n +2)
}

check_elf() {
  local bin="$1" line soname maxver key ceiling

  # NEEDED entries: the names the loader will look up.
  while read -r soname; do
    [ -n "$soname" ] || continue
    case "$soname" in
      libc.so.*|libm.so.*|libgcc_s.so.*|libdl.so.*|libpthread.so.*) ;;
      librt.so.*|libutil.so.*|ld-linux*.so.*) ;;
      *)
        fail "$bin has a non-baseline NEEDED entry: $soname"
        ;;
    esac
  done < <(readelf -d "$bin" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')

  # Resolved paths: catches a baseline soname that happens to resolve into a
  # build-host store on this machine.
  if command -v ldd >/dev/null 2>&1; then
    while read -r line; do
      case "$line" in
        */nix/store/*) fail "$bin resolves a dependency into the Nix store: ${line#"${line%%[![:space:]]*}"}" ;;
        *"not found"*) fail "$bin has an unresolved dependency: ${line#"${line%%[![:space:]]*}"}" ;;
      esac
    done < <(ldd "$bin" 2>/dev/null || true)
  fi

  # Highest GLIBC_x.y symbol version the binary demands.
  maxver="$(readelf -V "$bin" 2>/dev/null \
    | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' \
    | sort -t. -k1,1n -k2,2n | tail -n 1)"
  if [ -n "$maxver" ]; then
    key="$(ver_key "$maxver")"
    ceiling="$(ver_key "$GLIBC_MAX")"
    if [ "$key" -gt "$ceiling" ]; then
      fail "$bin requires GLIBC_$maxver, above the $GLIBC_MAX baseline (runner image drift?)"
    else
      printf 'portability: %s requires at most GLIBC_%s (baseline %s)\n' "$bin" "$maxver" "$GLIBC_MAX"
    fi
  fi
}

[ "$#" -gt 0 ] || {
  echo "usage: check-binary-portability.sh <binary> [<binary>...]" >&2
  exit 2
}

for bin in "$@"; do
  [ -f "$bin" ] || {
    fail "no such binary: $bin"
    continue
  }
  case "$(uname -s)" in
    Darwin) check_macho "$bin" ;;
    Linux) check_elf "$bin" ;;
    *)
      echo "portability: unsupported host $(uname -s); skipping" >&2
      exit 0
      ;;
  esac
done

if [ "$failures" -ne 0 ]; then
  printf 'portability: %d problem(s). Release binaries must run on a stock host.\n' "$failures" >&2
  exit 1
fi

printf 'portability: OK (%d binary/binaries)\n' "$#"
