# Release packaging and crates.io. Advisory product-CI-wise; workflow-gate
# still runs install-surface contracts.


# Every non-closed bead carries exactly one of rc-1.0 / post-1.0 — advisory, local-only.
[group('release')]
milestone-check:
    node scripts/check-milestone-labels.mjs


# Are any releases stuck? Reports drafts that never published, published
# releases with no assets, merged release PRs release-please never tagged, and
# manifest versions with no release. Needs an authenticated `gh`, so it is NOT
# in `ci` — it reads live GitHub state, not the working tree. release-drift.yml
# runs it daily; this is the same check, on demand.
[group('release')]
[doc('Report stuck releases: drafts, assetless publishes, untagged release PRs.')]
release-drift grace="120":
    GRACE_MINUTES={{ grace }} node scripts/check-release-drift.mjs


# Package the host-target release binaries into a tarball via
# scripts/pack-release.sh (phux-<tag>-<target>.tar.gz) under dist/. Used
# to seed the first Homebrew release locally; CI does this per-target on a
# `v*` tag. Pass the tag, e.g. `just dist v0.0.1`.
[group('release')]
[doc('Package host-target release binaries into dist/, matching release.yml naming.')]
dist TAG:
    bash scripts/dist.sh {{TAG}}


# Local release preflight before pressing the GitHub Actions release button.
# Runs version/tag checks, install-surface drift checks, formula generation,
# and a phux-protocol crates.io package dry-run.
[group('release')]
[doc('Release preflight: version/tag, install surface, formula, crate dry-run.')]
release-preflight TAG:
    bash scripts/release-preflight.sh {{TAG}}


# Same release preflight, but skip the crates.io dry-run when offline or when
# this is a binary/Homebrew-only release and cargo registry access is flaky.
[group('release')]
[doc('Release preflight without the crates.io dry-run, for offline or binary-only.')]
release-preflight-fast TAG:
    bash scripts/release-preflight.sh {{TAG}} --skip-crate-dry-run


# Check that a release tag matches the resolved Cargo package versions.
[group('release')]
release-check TAG:
    bash scripts/check-release-version.sh {{TAG}}


# Dry-run the crates.io publish of phux-protocol (package + verify, no
# upload). The only publishable crate. Mirrors the publish-crate workflow.
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


# PhuxFFI.xcframework: phux-client-ffi as a static library with the libghostty
# engine for iOS devices, the arm64 simulator and arm64 macOS, under a
# `PhuxFFI` module map, plus its provenance file (ADR-0133). Needs full Xcode
# with the iOS SDK, rustup and the pinned Zig; ffi-xcframework.yml attaches
# the same output to root releases. `just ffi-xcframework --dev` skips the
# device slice.
[group('release')]
[doc('Build PhuxFFI.xcframework (device, simulator, macOS) and its provenance file.')]
ffi-xcframework *ARGS:
    bash scripts/build-ffi-xcframework.sh {{ARGS}}

# The binding crate's UniFFI encoder (phux-client-ffi --features uniffi,
# ADR-0135). The artifact includes generated Swift from the same revision so
# consumers replace native slices and bindings atomically (phux-mobile
# ADR-0031).
[group('release')]
[doc('Build the canonical mobile UniFFI xcframework, bindings, and provenance.')]
mobile-ffi-xcframework *ARGS:
    bash scripts/build-mobile-ffi-xcframework.sh {{ARGS}}

# Android sibling of the same lane: Kotlin + arm64-v8a/x86_64 .so +
# provenance. ffi-android.yml uploads the zip; phux-mobile fetches it.
[group('release')]
[doc('Build the canonical Android UniFFI zip (Kotlin, cdylibs, provenance).')]
mobile-ffi-android:
    bash scripts/build-mobile-ffi-android.sh
