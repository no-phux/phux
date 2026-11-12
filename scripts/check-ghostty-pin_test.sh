#!/usr/bin/env bash
# Offline contract tests: source-only is honest, missing listed assets are not.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-pin-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/repo/scripts/lib" "$scratch/repo/.config" "$scratch/bin"
cp "$root/scripts/check-ghostty-pin.sh" "$scratch/repo/scripts/"
cp "$root/scripts/lib/output.sh" "$scratch/repo/scripts/lib/"
cp "$root/.config/zig-toolchain.json" "$scratch/repo/.config/"
rev=0123456789abcdef0123456789abcdef01234567
cat >"$scratch/repo/Cargo.lock" <<EOF
[[package]]
name = "libghostty-vt-sys"
source = "git+https://github.com/example/bindings.git?rev=$rev#$rev"
EOF
cat >"$scratch/build.rs" <<EOF
const GHOSTTY_REPO: &str = "https://github.com/example/engine.git";
const GHOSTTY_COMMIT: &str = "$rev";
EOF
cat >"$scratch/repo/scripts/build-vt-wasm.sh" <<EOF
revision=$rev
archive_sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
# https://codeload.github.com/example/engine/tar.gz/\$revision
EOF
version="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)",*/\1/p' "$root/.config/zig-toolchain.json")"
printf '.minimum_zig_version = "%s",\n' "$version" >"$scratch/build.zig.zon"
cat >"$scratch/download.rs" <<'EOF'
const PREBUILT_BASE_URL: &str = "https://github.com/example/bindings/releases/download";
const PREBUILT_OPTIMIZE: &str = "fast";
const PREBUILT_CPU: &str = "baseline";
const PREBUILT_ARCHIVES: &[(&str, &str)] = &[];
EOF
cp "$scratch/download.rs" "$scratch/empty.rs"
cat >"$scratch/bin/curl" <<'SH'
#!/usr/bin/env bash
set -eu
url="${!#}"
[[ "${PIN_TEST_OFFLINE:-0}" == 0 ]] || exit 7
case "$url" in
  */build.rs) cat "$PIN_TEST_ROOT/build.rs"; printf '\n200' ;;
  */download.rs) cat "$PIN_TEST_ROOT/download.rs"; printf '\n200' ;;
  */build.zig.zon)
    cat "$PIN_TEST_ROOT/build.zig.zon"
    printf '\n%s' "${PIN_TEST_SOURCE_HTTP:-200}" ;;
  */libghostty-vt-*.tar.gz) printf '%s' "${PIN_TEST_ASSET_HTTP:-200}" ;;
  *) exit 99 ;;
esac
SH
chmod +x "$scratch/bin/curl"
export PIN_TEST_ROOT="$scratch"
export PATH="$scratch/bin:$PATH"
run() { bash "$scratch/repo/scripts/check-ghostty-pin.sh" >"$scratch/output" 2>&1; }
pass() {
  run || { cat "$scratch/output" >&2; exit 1; }
  grep -Fq "$1" "$scratch/output" || { cat "$scratch/output" >&2; exit 1; }
}
fail() {
  if run; then cat "$scratch/output" >&2; echo 'unexpected pin success' >&2; exit 1; fi
  grep -Fq "$1" "$scratch/output" || { cat "$scratch/output" >&2; exit 1; }
}
pass '0 prebuilt, 4 source-only targets'
sed "s/?rev=$rev#/?rev=ffffffffffffffffffffffffffffffffffffffff#/" "$scratch/repo/Cargo.lock" >"$scratch/bad.lock"
mv "$scratch/repo/Cargo.lock" "$scratch/good.lock"
mv "$scratch/bad.lock" "$scratch/repo/Cargo.lock"
fail 'binding revision does not match'
mv "$scratch/good.lock" "$scratch/repo/Cargo.lock"
sed "s/revision=$rev/revision=bad/" "$scratch/repo/scripts/build-vt-wasm.sh" >"$scratch/bad.sh"
mv "$scratch/repo/scripts/build-vt-wasm.sh" "$scratch/good.sh"
mv "$scratch/bad.sh" "$scratch/repo/scripts/build-vt-wasm.sh"
fail 'differs from browser engine'
mv "$scratch/good.sh" "$scratch/repo/scripts/build-vt-wasm.sh"
PIN_TEST_OFFLINE=1 pass 'skipped'
PIN_TEST_SOURCE_HTTP=404 fail 'is not served (HTTP 404)'
printf '.minimum_zig_version = "0.0.0",\n' >"$scratch/build.zig.zon"
fail 'engine requires Zig 0.0.0'
printf '.minimum_zig_version = "%s.0",\n' "${version%.*}" >"$scratch/build.zig.zon"
pass '0 prebuilt, 4 source-only targets'
printf '.minimum_zig_version = "%s.99",\n' "${version%.*}" >"$scratch/build.zig.zon"
fail "engine requires Zig ${version%.*}.99"
printf '.minimum_zig_version = "%s",\n' "$version" >"$scratch/build.zig.zon"
sed "s/$rev/main/" "$scratch/build.rs" >"$scratch/bad.rs"
mv "$scratch/build.rs" "$scratch/good.rs"
mv "$scratch/bad.rs" "$scratch/build.rs"
fail 'not an immutable full commit'
mv "$scratch/good.rs" "$scratch/build.rs"
sed '/^const PREBUILT_ARCHIVES:/d' "$scratch/download.rs" >"$scratch/bad.rs"
mv "$scratch/bad.rs" "$scratch/download.rs"
fail 'prebuilt coverage table is missing'
cp "$scratch/empty.rs" "$scratch/download.rs"
sed 's/= &\[\];/= \&[("aarch64-apple-darwin", "bad")];/' "$scratch/download.rs" >"$scratch/bad.rs"
mv "$scratch/bad.rs" "$scratch/download.rs"
fail 'malformed prebuilt checksum'
digest=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
sed "s/\"bad\"/\"$digest\"/" "$scratch/download.rs" >"$scratch/good.rs"
mv "$scratch/good.rs" "$scratch/download.rs"
pass '1 prebuilt, 3 source-only targets'
PIN_TEST_ASSET_HTTP=404 fail 'is not served'
echo 'ghostty pin tests passed (immutable source, toolchain, source-only and actual prebuilt coverage)'
