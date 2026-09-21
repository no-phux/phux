#!/usr/bin/env bash
# Install NDK r28c (28.2.13676358) if ANDROID_NDK_HOME is unset or empty.
set -euo pipefail
if [[ -n "${ANDROID_NDK_HOME:-}" && -x "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/clang" ]]; then
    echo "ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
    exit 0
fi
root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/android-ndk"
mkdir -p "$root"
curl -fsSL -o "$root/ndk.zip" \
    "https://dl.google.com/android/repository/android-ndk-r28c-linux.zip"
unzip -q "$root/ndk.zip" -d "$root"
echo "ANDROID_NDK_HOME=$root/android-ndk-r28c"
