#!/usr/bin/env bash
# Fail when the release builder stops selecting its portable CPU floor. The
# static half (which surfaces call it) is tools/src/policy/release-cpu-baselines.test.ts.
set -euo pipefail
# shellcheck source=dev/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/../dev/lib/output.sh"

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Run the real builder against a fake cargo in a scratch directory, so the
# receipt it writes never lands in this checkout's target/.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/work"
cat > "$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
mkdir -p target/release
printf '%s\n%s\n%s\n' "$RUSTFLAGS" "$LIBGHOSTTY_VT_SYS_CPU" "$*"
EOF
chmod +x "$tmp/bin/cargo"
build() { (cd "$tmp/work" && PATH="$tmp/bin:$PATH" bash "$ROOT/packaging/build-release-binaries.sh" "$1"); }
check_target() {
  output="$(build "$1")"
  grep -Fxq -- "$2" <<<"$output"
  grep -Fxq baseline <<<"$output"
  grep -Fq -- 'build --locked --release --bin phux --bin phux-mcp' <<<"$output"
  grep -Fxq -- "$1 rust=$3 libghostty=baseline" "$tmp/work/target/release/.phux-cpu-baseline"
}
check_target x86_64-unknown-linux-gnu '-C target-cpu=x86-64 -C link-arg=-fuse-ld=mold' x86-64
check_target aarch64-unknown-linux-gnu '-C target-cpu=generic -C link-arg=-fuse-ld=mold' generic
check_target aarch64-apple-darwin '-C target-cpu=apple-m1' apple-m1
if build x86_64-apple-darwin >/dev/null 2>&1; then
  phux_error 'release CPU baseline drift: unsupported target was accepted'
  exit 1
fi

phux_ok 'release CPU baseline contracts passed'
