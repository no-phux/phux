#!/usr/bin/env bash
# Exercise fresh-PATH behavior and compiler download integrity without network.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-setup-test.XXXXXX")"
scratch="$(cd "$scratch" && pwd)"
trap 'rm -rf "$scratch"' EXIT
repo="$scratch/repo"
bin="$scratch/bin"
mkdir -p "$repo/scripts/lib" "$repo/.config" "$bin"
cp "$root"/scripts/{doctor,setup-rust,install-zig}.sh "$repo/scripts/"
cp "$root/scripts/lib/dev-toolchain.sh" "$repo/scripts/lib/"
cp "$root/rust-toolchain.toml" "$repo/"
cp "$root/.config/zig-toolchain.json" "$repo/.config/"
source "$root/scripts/lib/dev-toolchain.sh"
bash_bin="$BASH"
for tool in dirname basename sed head awk git grep find sort mkdir mktemp rm mv tar uname tr; do
    ln -s "$(command -v "$tool")" "$bin/$tool"
done
if command -v sha256sum >/dev/null 2>&1; then
    ln -s "$(command -v sha256sum)" "$bin/sha256sum"
else
    ln -s "$(command -v shasum)" "$bin/shasum"
fi
expect_pass() {
    if ! PATH="$bin" "$bash_bin" "$@" >"$scratch/output" 2>&1; then
        cat "$scratch/output" >&2; echo "unexpected failure: $*" >&2; exit 1
    fi
}
expect_fail() {
    local expected="$1"
    shift
    if PATH="$bin" "$bash_bin" "$@" >"$scratch/output" 2>&1; then
        echo "unexpected success: $*" >&2; exit 1
    fi
    grep -Fq "$expected" "$scratch/output" || { cat "$scratch/output" >&2; exit 1; }
}

# A docs contributor has only shell/Git utilities; no compiler or Node probes.
expect_pass "$repo/scripts/doctor.sh" docs
cp "$root/scripts/check-docs.sh" "$repo/scripts/"
mkdir -p "$repo/docs/adr"
touch "$repo/docs/adr/0008-first.md" "$repo/docs/adr/0009-second.md"
printf '| [0008](./0008-first.md) | First |\n| [0009](./0009-second.md) | Second |\n' >"$repo/docs/adr/README.md"
expect_pass "$repo/scripts/check-docs.sh" --only=adr-index-sync
expect_pass "$repo/scripts/check-docs.sh" --only=adr-number-unique
touch "$repo/docs/adr/0008-duplicate.md"
expect_fail 'ADR number 0008 is also used' "$repo/scripts/check-docs.sh" --only=adr-number-unique
rm -f "$repo/docs/adr/0008-duplicate.md"
printf '| [0008](./0008-first.md) | Duplicate |\n' >>"$repo/docs/adr/README.md"
expect_fail 'more than one row' "$repo/scripts/check-docs.sh" --only=adr-index-sync

# spec-id-unique: the spec allocation tables are registries on the wire ID
# column - a bare message table and appendix-reserved.md's backticked
# command-tag table cover the two row shapes.
mkdir -p "$repo/docs/spec"
cat >"$repo/docs/spec/proto.md" <<'MD'
| ID    | Direction | Name     | Reference | Status  |
|-------|-----------|----------|-----------|---------|
| 0x01  | C → S     | `HELLO`  | §6.1      | shipped |
| 0x02  | C → S     | `ATTACH` | §7.1      | shipped |
MD
cat >"$repo/docs/spec/appendix-reserved.md" <<'MD'
| Tag    | Command       | Owner            | Status  |
|--------|---------------|------------------|---------|
| `0x07` | `GET_SCREEN`  | [L1.md](./L1.md) | shipped |
| `0x08` | `ROUTE_INPUT` | [L1.md](./L1.md) | shipped |
MD
expect_pass "$repo/scripts/check-docs.sh" --only=spec-id-unique
printf '| 0x01  | C → S     | `BOGUS`  | §6.1      | shipped |\n' >>"$repo/docs/spec/proto.md"
expect_fail 'more than one row for message ID 0x01' "$repo/scripts/check-docs.sh" --only=spec-id-unique
cat >"$repo/docs/spec/proto.md" <<'MD'
| ID    | Direction | Name     | Reference | Status  |
|-------|-----------|----------|-----------|---------|
| 0x01  | C → S     | `HELLO`  | §6.1      | shipped |
| 0x02  | C → S     | `ATTACH` | §7.1      | shipped |
MD
printf '| `0x07` | `BOGUS`       | [L1.md](./L1.md) | shipped |\n' >>"$repo/docs/spec/appendix-reserved.md"
expect_fail 'more than one row for command tag 0x07' "$repo/scripts/check-docs.sh" --only=spec-id-unique
expect_fail 'usage:' "$repo/scripts/doctor.sh" typo
expect_fail 'bash scripts/setup-rust.sh core' "$repo/scripts/doctor.sh" core
rm -f "$bin/uname"
cat >"$bin/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac
SH
cat >"$bin/rustc" <<SH
#!/bin/sh
test "\$RUSTUP_AUTO_INSTALL" = 0 || exit 1
if [ "\$1" = --print ]; then echo "\$SETUP_TEST_SYSROOT"; else echo 'rustc $RUST_CHANNEL (fixture)'; fi
SH
for tool in cargo cc mold pkg-config npm; do
    cat >"$bin/$tool" <<'SH'
#!/bin/sh
exit 0
SH
done
cat >"$bin/node" <<'SH'
#!/bin/sh
echo 22
SH
chmod +x "$bin"/{uname,rustc,cargo,cc,mold,pkg-config,npm,node}
expect_pass "$repo/scripts/doctor.sh" core
expect_fail 'Node 24+' "$repo/scripts/doctor.sh" integrations
sed 's/echo 22/echo 24/' "$bin/node" >"$scratch/node"
cp "$scratch/node" "$bin/node"
expect_pass "$repo/scripts/doctor.sh" integrations
expect_fail "Zig $ZIG_VERSION" "$repo/scripts/doctor.sh" native
cat >"$bin/zig" <<'SH'
#!/bin/sh
echo 0.0.0
SH
chmod +x "$bin/zig"
expect_fail 'found: 0.0.0' "$repo/scripts/doctor.sh" native
sed "s/0.0.0/$ZIG_VERSION/" "$bin/zig" >"$scratch/zig"
cp "$scratch/zig" "$bin/zig"
expect_pass "$repo/scripts/doctor.sh" native
# Browser-client work reuses its committed engine; Zig must not be required.
export SETUP_TEST_SYSROOT="$scratch/sysroot"
mkdir -p "$SETUP_TEST_SYSROOT/lib/rustlib/wasm32-unknown-unknown/lib" "$repo/clients/phux-web" "$repo/clients/phux-vt-web/vendor"
cp "$root/clients/phux-web/Cargo.toml" "$repo/clients/phux-web/"
touch "$repo/clients/phux-vt-web/vendor/ghostty-vt.wasm"
bindgen="$(sed -n 's/^wasm-bindgen = "=\([^"]*\)"/\1/p' "$root/clients/phux-web/Cargo.toml")"
cat >"$bin/wasm-bindgen" <<SH
#!/bin/sh
echo 'wasm-bindgen $bindgen'
SH
chmod +x "$bin/wasm-bindgen"
cp "$bin/npm" "$bin/wasm-pack"
cp "$bin/npm" "$bin/wasm-opt"
rm -f "$bin/zig"
expect_pass "$repo/scripts/doctor.sh" web
cp "$scratch/zig" "$bin/zig"
chmod +x "$bin/zig"
rm -f "$bin/mold"
expect_fail 'mold' "$repo/scripts/doctor.sh" core

# macOS must diagnose an SDK with no nmedit before starting the VT build.
cat >"$bin/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; esac
SH
cat >"$bin/xcrun" <<'SH'
#!/bin/sh
test "$1" = --show-sdk-path
SH
chmod +x "$bin/xcrun"
expect_fail 'macOS SDK/nmedit' "$repo/scripts/doctor.sh" native

# Opt-in rustup components must not leak into a core contributor's install.
export SETUP_TEST_LOG="$scratch/rustup-log"
cat >"$bin/rustup" <<'SH'
#!/bin/sh
echo "$*" >>"$SETUP_TEST_LOG"
SH
chmod +x "$bin/rustup"
expect_pass "$repo/scripts/setup-rust.sh" core
grep -Fq "toolchain install $RUST_CHANNEL --profile minimal" "$SETUP_TEST_LOG"
if grep -Eq 'wasm32|llvm-tools|rust-analyzer|default ' "$SETUP_TEST_LOG"; then exit 1; fi
expect_pass "$repo/scripts/setup-rust.sh" web
grep -Fq "target add --toolchain $RUST_CHANNEL wasm32-unknown-unknown" "$SETUP_TEST_LOG"

# A corrupt download must never be extracted or published. Use a fixture pin
# for the valid path, with a real tarball and real SHA verification.
export SETUP_TEST_ARCHIVE="$scratch/download.tar.xz"
export SETUP_TEST_DOWNLOADS="$scratch/downloads"
cat >"$bin/curl" <<'SH'
#!/bin/sh
echo download >>"$SETUP_TEST_DOWNLOADS"
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then cp "$SETUP_TEST_ARCHIVE" "$2"; exit; fi
    shift
done
exit 1
SH
ln -s "$(command -v cp)" "$bin/cp"
chmod +x "$bin/curl"
printf 'corrupt archive' >"$SETUP_TEST_ARCHIVE"
expect_fail 'checksum' "$repo/scripts/install-zig.sh" "$scratch/toolchains"
[[ ! -e "$scratch/toolchains/zig-aarch64-macos-$ZIG_VERSION" ]]
mkdir -p "$scratch/payload/zig-aarch64-macos-$ZIG_VERSION"
cp "$bin/zig" "$scratch/payload/zig-aarch64-macos-$ZIG_VERSION/zig"
tar -cf "$SETUP_TEST_ARCHIVE" -C "$scratch/payload" "zig-aarch64-macos-$ZIG_VERSION"
if command -v sha256sum >/dev/null 2>&1; then
    sha="$(sha256sum "$SETUP_TEST_ARCHIVE" | awk '{print $1}')"
else
    sha="$(shasum -a 256 "$SETUP_TEST_ARCHIVE" | awk '{print $1}')"
fi
cat >"$repo/.config/zig-toolchain.json" <<SH
{
  "version": "$ZIG_VERSION",
  "archives": {
    "aarch64-macos": {
      "archive": "zig-aarch64-macos-$ZIG_VERSION.tar.xz",
      "sha256": "$sha"
    }
  }
}
SH
expect_pass "$repo/scripts/install-zig.sh" "$scratch/toolchains"
grep -Fxq "$scratch/toolchains/zig-aarch64-macos-$ZIG_VERSION" "$scratch/output"
rm -f "$SETUP_TEST_DOWNLOADS"
expect_pass "$repo/scripts/install-zig.sh" "$scratch/toolchains"
[[ ! -e "$SETUP_TEST_DOWNLOADS" ]]
# Engine regeneration rejects corrupt source before building, preserves the
# old artifact on ABI failure, and never replaces it in comparison mode.
cp "$root/scripts/build-vt-wasm.sh" "$repo/scripts/"
ln -s "$(command -v cmp)" "$bin/cmp"
ln -s "$(command -v du)" "$bin/du"
ln -s "$(command -v cut)" "$bin/cut"
engine="$repo/clients/phux-vt-web/vendor/ghostty-vt.wasm"
printf old >"$engine"
printf corrupt >"$SETUP_TEST_ARCHIVE"
expect_fail 'source checksum mismatch' "$repo/scripts/build-vt-wasm.sh"
[[ "$(cat "$engine")" = old ]]
export GHOSTTY_SRC="$scratch/local-engine"
mkdir -p "$GHOSTTY_SRC"
touch "$GHOSTTY_SRC/build.zig"
cat >"$bin/zig" <<SH
#!/bin/sh
if [ "\$1" = version ]; then echo '$ZIG_VERSION'; exit; fi
while [ "\$#" -gt 0 ]; do
  if [ "\$1" = --prefix ]; then
    mkdir -p "\$2/bin"
    printf rebuilt >"\$2/bin/ghostty-vt.wasm"
    exit
  fi
  shift
done
exit 1
SH
cat >"$bin/node" <<'SH'
#!/bin/sh
echo 'fixture ABI rejected'
exit 1
SH
expect_fail 'ABI rejected' "$repo/scripts/build-vt-wasm.sh"
[[ "$(cat "$engine")" = old ]]
cp "$bin/npm" "$bin/node"
expect_fail 'engine differs' "$repo/scripts/build-vt-wasm.sh" --check
[[ "$(cat "$engine")" = old ]]
expect_pass "$repo/scripts/build-vt-wasm.sh"
[[ "$(cat "$engine")" = rebuilt ]]
expect_pass "$repo/scripts/build-vt-wasm.sh" --check
unset GHOSTTY_SRC
echo 'dev setup tests passed (scoped PATH, version/SDK errors, opt-in components, checksum rejection, idempotence)'
