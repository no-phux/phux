# shellcheck shell=bash
# Sourced by test-install.sh after its installing section: install.sh on
# Debian, Ubuntu, Fedora and RHEL adds phux's package repository. Fakes stand
# in for uname, id (root, so sudo never runs) and the package tools; nothing
# reaches the real system. scripts/test-linux-packages.sh runs it for real.
[[ $HOME == "$TMP"/* ]] || { echo "source this from scripts/test-install.sh" >&2; exit 1; }
# --- package repositories (Linux) -----------------------------------------------

mkdir -p "$TMP/repo-bin"
printf '#!/bin/sh\ncase "$1" in -m) echo x86_64 ;; *) echo Linux ;; esac\n' > "$TMP/repo-bin/uname"
printf '#!/bin/sh\necho 0\n' > "$TMP/repo-bin/id"
for tool in apt-get dnf rpm install; do
  printf '#!/bin/sh\necho "%s $*" >> "$REPO_CALLS"\n[ "%s" != "${REPO_FAIL:-}" ]\n' "$tool" "$tool" \
    > "$TMP/repo-bin/$tool"
done
chmod 755 "$TMP/repo-bin/"*
export REPO_CALLS="$TMP/repo-calls"
os_release() { printf 'ID=%s\nID_LIKE="%s"\n' "$1" "$2" > "$TMP/os-release"; }
repo() {
  : > "$REPO_CALLS"
  TEST_PATH="$TMP/repo-bin:/usr/bin:/bin" PHUX_OS_RELEASE="$TMP/os-release" PHUX_NO_REPO="${NO_REPO:-}" \
    installer --no-modify-path "$@" >"$TMP/out" 2>"$TMP/err"
}
REPO='https://github.com/no-phux/phux/releases/download'

os_release ubuntu debian
repo --dry-run && grep -Fxq "repository: apt $REPO" "$TMP/out" || fail "ubuntu plan: $(cat "$TMP/out")"
repo || fail "apt install: $(cat "$TMP/err")"
grep -Fq 'install -m 644' "$REPO_CALLS" && grep -Fq '/etc/apt/sources.list.d/phux.list' "$REPO_CALLS" \
  && grep -Fq 'apt-get install -y -qq phux' "$REPO_CALLS" || fail "apt calls: $(cat "$REPO_CALLS")"
grep -Fq 'adding the phux apt repository' "$TMP/err" || fail "the repository is added silently"
for distro in 'fedora:' 'rocky:rhel centos fedora'; do
  os_release "${distro%%:*}" "${distro#*:}"
  repo || fail "dnf install on ${distro%%:*}: $(cat "$TMP/err")"
  grep -Fq '/etc/yum.repos.d/phux.repo' "$REPO_CALLS" && grep -Fq 'rpm --import' "$REPO_CALLS" \
    && grep -Fq 'dnf install -y -q phux' "$REPO_CALLS" || fail "dnf calls: $(cat "$REPO_CALLS")"
done
# Arch, opt-outs, a chosen place or platform, and other channels or pins: the archive.
os_release arch ''
repo --dry-run && grep -Fq 'url: ' "$TMP/out" || fail "arch takes the archive: $(cat "$TMP/out")"
os_release ubuntu debian
for opt_out in --no-repo NO_REPO=1 '--install-dir x' '--channel next' '--version 9.8.7' '--channel lts'; do
  # shellcheck disable=SC2086 # each opt-out is its own words
  if [[ $opt_out == NO_REPO=1 ]]; then NO_REPO=1 repo --dry-run; else repo --dry-run $opt_out; fi
  ! grep -Fq 'repository:' "$TMP/out" || fail "$opt_out still adds the repository"
done
# A repository that fails falls back to the archive, and says so.
REPO_FAIL=apt-get repo || fail "fallback: $(cat "$TMP/err")"
grep -Fq 'installing the release archive instead' "$TMP/err" && grep -Fxq 'phux v9.8.7' "$HOME/.local/bin/phux" \
  || fail "no archive fallback: $(cat "$TMP/err")"
