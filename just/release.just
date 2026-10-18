# Release operations. CI does the releasing; see docs/RELEASING.md.


# Every non-closed bead carries exactly one of rc-1.0 / post-1.0 — advisory, local-only.
[group('release')]
milestone-check:
    bun tools/main.ts milestone-labels


# Stuck drafts, assetless releases, untagged release PRs, a stale stable
# manifest. Reads live GitHub state through an authenticated `gh`.
[group('release')]
[doc('Report stuck releases: drafts, assetless publishes, untagged release PRs.')]
release-drift grace="120":
    GRACE_MINUTES={{ grace }} bun tools/main.ts release-drift


# Dry-run the crates.io publish of phux-protocol, the only publishable crate.
[group('release')]
[doc('Dry-run the phux-protocol crates.io publish: package and verify, no upload.')]
publish-protocol-dry:
    cargo publish --locked --dry-run -p phux-protocol


# Publish phux-protocol to crates.io. IRREVERSIBLE. Requires `cargo login`
# (or CARGO_REGISTRY_TOKEN). Run `just publish-protocol-dry` first.
[group('release')]
[doc('Publish phux-protocol to crates.io. IRREVERSIBLE.')]
[confirm('Publish phux-protocol to crates.io? This cannot be undone. [y/N]')]
publish-protocol:
    cargo publish --locked -p phux-protocol


# Write the product skills package (the no-phux/skills publish tree).
[group('release')]
[doc('Write the product skills package to DEST (publish mirror of .agents/skills).')]
sync-skills DEST:
    bash scripts/export-product-skills.sh "{{DEST}}"


# PhuxFFI.xcframework (iOS device, arm64 simulator, macOS) plus provenance
# (ADR-0133). Needs full Xcode with the iOS SDK, rustup and the pinned Zig;
# `--dev` skips the device slice.
[group('release')]
[doc('Build PhuxFFI.xcframework (device, simulator, macOS) and its provenance file.')]
ffi-xcframework *ARGS:
    bash scripts/build-ffi-xcframework.sh {{ARGS}}

# The UniFFI encoder's xcframework, with generated Swift from the same revision
# so consumers replace slices and bindings together (ADR-0135).
[group('release')]
[doc('Build the canonical mobile UniFFI xcframework, bindings, and provenance.')]
mobile-ffi-xcframework *ARGS:
    bash scripts/build-mobile-ffi-xcframework.sh {{ARGS}}

# The Android sibling: Kotlin, arm64-v8a/x86_64 .so, and provenance.
[group('release')]
[doc('Build the canonical Android UniFFI zip (Kotlin, cdylibs, provenance).')]
mobile-ffi-android:
    bash scripts/build-mobile-ffi-android.sh
