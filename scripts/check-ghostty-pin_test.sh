#!/usr/bin/env bash
# Offline fixtures for explicit source builds and the default archive policy.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-ghostty-pin-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/repo/scripts/lib" "$scratch/repo/.cargo" "$scratch/bin"
cp "$repo/scripts/check-ghostty-pin.sh" "$scratch/repo/scripts/"
cp "$repo/scripts/lib/output.sh" "$scratch/repo/scripts/lib/"
cat >"$scratch/repo/Cargo.lock" <<'LOCK'
[[package]]
name = "libghostty-vt-sys"
source = "git+https://github.com/phall1/libghostty-rs.git?rev=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
LOCK
cat >"$scratch/build.rs" <<'BUILD'
const GHOSTTY_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
BUILD
cat >"$scratch/download.rs" <<'DOWNLOAD'
const PREBUILT_BASE_URL: &str = "https://github.com/phall1/libghostty-rs/releases/download";
const PREBUILT_OPTIMIZE: &str = "ReleaseFast";
const PREBUILT_CPU: &str = "baseline";
const PREBUILT_ARCHIVES: &[(&str, &str)] = &[];
DOWNLOAD
cat >"$scratch/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  */build.rs*) cat "$GHOSTTY_PIN_FIXTURES/build.rs" ;;
  */download.rs*) cat "$GHOSTTY_PIN_FIXTURES/download.rs" ;;
  *releases/download*) printf '200' ;;
  *) exit 44 ;;
esac
CURL
chmod +x "$scratch/bin/curl"
export PATH="$scratch/bin:$PATH" GHOSTTY_PIN_FIXTURES="$scratch"

check() {
  local expected="$1" message="$2" actual=0
  bash "$scratch/repo/scripts/check-ghostty-pin.sh" >"$scratch/result" 2>&1 || actual=$?
  if [[ "$actual" != "$expected" ]] || ! grep -q "$message" "$scratch/result"; then
    cat "$scratch/result" >&2
    echo "ghostty pin fixture: expected status $expected and '$message', got $actual" >&2
    exit 1
  fi
}

printf '[env]\nLIBGHOSTTY_VT_SYS_FROM_SOURCE = { value = "1", force = true }\n' >"$scratch/repo/.cargo/config.toml"
check 0 'explicitly builds from source'
printf '[env]\nLIBGHOSTTY_VT_SYS_FROM_SOURCE = { value = "1", force = true } # deliberate\n' >"$scratch/repo/.cargo/config.toml"
check 0 'explicitly builds from source'
printf '[env]\nLIBGHOSTTY_VT_SYS_FROM_SOURCE = "1"\n' >"$scratch/repo/.cargo/config.toml"
check 1 'publishes no prebuilt'
printf '[env]\n' >"$scratch/repo/.cargo/config.toml"
check 1 'publishes no prebuilt'
printf '[env]\n# LIBGHOSTTY_VT_SYS_FROM_SOURCE = { value = "1", force = true }\n' >"$scratch/repo/.cargo/config.toml"
check 1 'publishes no prebuilt'
printf '[env]\nLIBGHOSTTY_VT_SYS_FROM_SOURCE = "0"\n' >"$scratch/repo/.cargo/config.toml"
export LIBGHOSTTY_VT_SYS_FROM_SOURCE=1
check 1 'publishes no prebuilt'
printf '[build]\nLIBGHOSTTY_VT_SYS_FROM_SOURCE = { value = "1", force = true }\n' >"$scratch/repo/.cargo/config.toml"
check 1 'publishes no prebuilt'

# The original prebuilt path still requires every target, not a partial table.
printf '[env]\n' >"$scratch/repo/.cargo/config.toml"
digest="$(printf '%064d' 0)"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin; do
  printf '("%s", "%s"),\n' "$target" "$digest" >>"$scratch/download.rs"
done
check 1 'publishes no prebuilt.*x86_64-apple-darwin'
printf '("x86_64-apple-darwin", "%s"),\n' "$digest" >>"$scratch/download.rs"
check 0 'publishes ghostty-vt-'
echo 'ghostty pin fixtures: 9 cases passed'
