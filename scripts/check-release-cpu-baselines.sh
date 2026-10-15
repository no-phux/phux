#!/usr/bin/env bash
# Fail when a public native build surface stops selecting its portable CPU floor.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

require() {
  grep -Fq -- "$2" "$1" || {
    phux_error "release CPU baseline drift: $1 is missing $2"
    exit 1
  }
}

for workflow in .github/workflows/release.yml .github/workflows/next-release.yml; do
  require "$workflow" 'bash scripts/build-release-binaries.sh "${{ matrix.target }}"'
done
require docs/site/worker/Dockerfile 'RUSTFLAGS="-C target-cpu=x86-64"'
require docs/site/worker/Dockerfile 'LIBGHOSTTY_VT_SYS_CPU=baseline'
# The PhuxFFI xcframework lane: an explicit Rust floor per Apple target and
# the engine on baseline, with no inherited RUSTFLAGS reaching a slice.
require scripts/build-ffi-xcframework.sh 'target-cpu=apple-a7'
require scripts/build-ffi-xcframework.sh 'target-cpu=apple-a12'
require scripts/build-ffi-xcframework.sh 'target-cpu=apple-m1'
require scripts/build-ffi-xcframework.sh 'LIBGHOSTTY_VT_SYS_CPU=baseline'
require scripts/build-ffi-xcframework.sh 'unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS'
require .github/workflows/ffi-xcframework.yml 'bash scripts/build-ffi-xcframework.sh --full'

# Exercise the canonical builder without compiling. This pins every target's
# exact Rust floor, the native-engine floor, and rejection of unknown targets.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/cargo" <<'EOF'
#!/usr/bin/env bash
mkdir -p target/release
printf '%s\n%s\n%s\n' "$RUSTFLAGS" "$LIBGHOSTTY_VT_SYS_CPU" "$*"
EOF
chmod +x "$tmp/cargo"
check_target() {
  output="$(PATH="$tmp:$PATH" bash scripts/build-release-binaries.sh "$1")"
  grep -Fxq -- "$2" <<<"$output"
  grep -Fxq baseline <<<"$output"
  grep -Fq -- 'build --locked --release --bin phux --bin phux-mcp' <<<"$output"
  grep -Fxq -- "$1 rust=$3 libghostty=baseline" target/release/.phux-cpu-baseline
}
check_target x86_64-unknown-linux-gnu '-C target-cpu=x86-64 -C link-arg=-fuse-ld=mold' x86-64
check_target aarch64-unknown-linux-gnu '-C target-cpu=generic -C link-arg=-fuse-ld=mold' generic
check_target aarch64-apple-darwin '-C target-cpu=apple-m1' apple-m1
if PATH="$tmp:$PATH" bash scripts/build-release-binaries.sh x86_64-apple-darwin >/dev/null 2>&1; then
  phux_error 'release CPU baseline drift: unsupported target was accepted'
  exit 1
fi

phux_ok 'release CPU baseline contracts passed'
