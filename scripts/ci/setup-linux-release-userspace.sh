#!/usr/bin/env bash
# Bootstrap an Ubuntu 22.04 job container so release.yml / next-release.yml
# can keep a native glibc 2.35 floor on ubuntu-24.04 / ubuntu-24.04-arm hosts
# after the hosted ubuntu-22.04 labels retire (actions/runner-images#14254).
#
# Runs as root inside ubuntu:22.04. stdout is logs only.
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
    echo "Linux release userspace setup is a no-op on $(uname -s)" >&2
    exit 0
fi

if [[ "$(id -u)" -ne 0 ]]; then
    echo "this setup must run as root in the Ubuntu 22.04 job container" >&2
    exit 1
fi

export DEBIAN_FRONTEND=noninteractive

apt-get update
apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    curl \
    g++-12 \
    gcc-12 \
    git \
    jq \
    mold \
    pkg-config \
    xz-utils

# rustc passes -fuse-ld=mold to cc. Jammy's default GCC 11 rejects that name;
# GCC 12 accepts it and is what the hosted 22.04 image also shipped.
ln -sfn "$(command -v gcc-12)" /usr/local/bin/cc
ln -sfn "$(command -v g++-12)" /usr/local/bin/c++

if [[ -n "${GITHUB_ENV:-}" ]]; then
    {
        echo "CC=gcc-12"
        echo "CXX=g++-12"
    } >> "$GITHUB_ENV"
fi

if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain none
fi

cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [[ -n "${GITHUB_PATH:-}" ]]; then
    echo "$cargo_bin" >> "$GITHUB_PATH"
fi
PATH="$cargo_bin:$PATH"
export PATH

command -v rustup >/dev/null
command -v mold >/dev/null
gcc-12 -fuse-ld=mold -x c -o /tmp/phux-mold-check - <<'EOF'
int main(void) { return 0; }
EOF
rm -f /tmp/phux-mold-check
