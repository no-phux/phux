#!/usr/bin/env bash
# Build distributable CLI binaries with explicit Rust and libghostty CPU floors.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

target="${1:?usage: build-release-binaries.sh <target>}"
case "$target" in
  x86_64-unknown-linux-gnu)
    rust_cpu=x86-64
    rustflags='-C target-cpu=x86-64 -C link-arg=-fuse-ld=mold'
    ;;
  aarch64-unknown-linux-gnu)
    rust_cpu=generic
    rustflags='-C target-cpu=generic -C link-arg=-fuse-ld=mold'
    ;;
  aarch64-apple-darwin)
    rust_cpu=apple-m1
    rustflags='-C target-cpu=apple-m1'
    ;;
  *)
    phux_error "no portable release CPU baseline for $target"
    exit 2
    ;;
esac

export RUSTFLAGS="$rustflags"
export LIBGHOSTTY_VT_SYS_CPU=baseline
# Marks a distributed artifact (phux_config::instance::BuildKind::Release). A
# stamped server only hot-swaps into another stamped build, so a dev or
# local build copied over the installed binary can never replace it.
export PHUX_RELEASE_ARTIFACT=1
phux_info "release CPU baseline: target=$target rust=$rust_cpu libghostty=baseline"
receipt=target/release/.phux-cpu-baseline
rm -f -- "$receipt"
cargo build --locked --release --bin phux --bin phux-mcp
printf '%s rust=%s libghostty=baseline\n' "$target" "$rust_cpu" \
  > "$receipt"
