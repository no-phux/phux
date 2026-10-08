#!/usr/bin/env bash
# Headless host/CoreText test, not a screenshot or a production app launch.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SDK_HASH="$(python3 - "${ROOT}/build.zig.zon" <<'PY'
import re
import sys
from pathlib import Path
block = Path(sys.argv[1]).read_text().split('.native_sdk = .{', 1)[1].split('},', 1)[0]
print(re.search(r'\.hash\s*=\s*"([^"]+)"', block).group(1))
PY
)"
SDK="${ROOT}/zig-pkg/${SDK_HASH}"
[[ -f "${SDK}/src/platform/macos/appkit_host.m" ]] || {
    printf 'error: run scripts/zig-build.sh test first to resolve the pinned SDK\n' >&2
    exit 1
}
python3 "${ROOT}/scripts/verify-paper-mono.py"
BIN="$(mktemp -d)/paper-mono-host-check"
trap 'rm -rf "$(dirname "$BIN")"' EXIT
clang -w -fobjc-arc -fno-sanitize=builtin -ObjC -mmacosx-version-min=11.0 \
    -DNATIVE_SDK_APPKIT_HOST="\"${SDK}/src/platform/macos/appkit_host.m\"" \
    -o "$BIN" "${ROOT}/scripts/paper-mono-host-check.m" \
    -framework Foundation -framework AppKit -framework Metal \
    -framework QuartzCore -framework CoreText -framework CoreGraphics \
    -framework ImageIO -framework AVFoundation -framework UniformTypeIdentifiers \
    -framework WebKit -framework Security -framework ScreenCaptureKit \
    -framework CoreMedia -framework CoreVideo -framework IOKit -framework Carbon \
    -framework Accelerate -framework MediaToolbox
printf 'sdk: %s\n' "${SDK_HASH}"
"$BIN" "${ROOT}/src/fonts"
