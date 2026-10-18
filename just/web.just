# Browser clients: clients/phux-web and clients/phux-vt-web. CI runs this lane
# inside `nix develop .#browser`, which adds a Chrome matched to chromedriver.


# The `web` lane. PHUX_WEB_ENGINE=1 also reproduces the committed WASM engine
# from its pinned Zig release (the official binary, never the shell's zig).
[group('web')]
[doc('Browser lane: engine ABI, wasm-pack tests, shipping package, headless Chrome.')]
web-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${PHUX_WEB_ENGINE:-0}" == 1 ]]; then bash scripts/build-vt-wasm.sh --check; fi
    node --test scripts/test-vt-wasm.mjs
    (cd clients/phux-vt-web && wasm-pack test --node --locked)
    (cd clients/phux-web && wasm-pack test --node --locked)
    (cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg --locked)
    bun tools/main.ts web-browser
