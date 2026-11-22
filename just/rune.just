# The Rune client (clients/rune). Its workspace and lock are its own, so these
# are the only recipes that reach it; rune-build.yml runs the same lines.
[group('rune')]
[doc('Format check, clippy, tests and release-rule tests for the Rune client')]
rune-check: worktree-claim
    bash clients/rune/scripts/check.sh

[group('rune')]
[doc('Paint every snapshot gallery into DIR: chrome, settings, onboarding, phone navigation, resources')]
rune-gallery DIR: worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    dir="$(mkdir -p "{{DIR}}" && cd "{{DIR}}" && pwd)"
    mkdir -p "$dir/gallery" "$dir/settings" "$dir/onboarding" "$dir/navigation"
    cd clients/rune
    RUNE_GALLERY_SNAP="$dir/gallery" RUNE_SETTINGS_SNAP="$dir/settings" \
        RUNE_ONBOARDING_SNAP="$dir/onboarding" RUNE_NAVIGATION_SNAP="$dir/navigation" \
        RUNE_RESOURCE_SNAP="$dir/resource.png" \
        cargo test --locked --all-features -- gallery settings_native_render_frames every_step_renders_its_parts navigation resource

[group('rune')]
[doc('Film the themes demo headless into clients/rune/demo/themes.mp4 (needs ffmpeg)')]
rune-demo: build
    bash clients/rune/scripts/demo.sh target/debug/phux

[group('rune')]
[doc("Advisories, licenses and sources over Rune's own lockfile")]
rune-deny:
    cargo-deny --manifest-path clients/rune/Cargo.toml --config clients/rune/deny.toml --locked check
