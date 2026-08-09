#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

failures=0

require_fixed() {
  local file="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$ROOT/$file"; then
    printf 'missing: %s: %s\n' "$file" "$needle" >&2
    failures=$((failures + 1))
  fi
}

forbid_fixed() {
  local file="$1"
  local needle="$2"
  if grep -Fq -- "$needle" "$ROOT/$file"; then
    printf 'stale claim: %s: %s\n' "$file" "$needle" >&2
    failures=$((failures + 1))
  fi
}

require_regex() {
  local file="$1"
  local regex="$2"
  if ! grep -Eq -- "$regex" "$ROOT/$file"; then
    printf 'missing regex: %s: %s\n' "$file" "$regex" >&2
    failures=$((failures + 1))
  fi
}

# The README is a landing page: it carries the brew one-liner, the platform
# truth, and a pointer to INSTALL.md. The full channel matrix, source builds,
# and cargo-install caveats are gated on docs/INSTALL.md below.
require_fixed README.md "Quick start"
require_fixed README.md "brew install phall1/tap/phux"
forbid_fixed README.md "brew install phall1/phux/phux"
require_fixed README.md "macOS arm64, Linux x86_64, and Linux arm64"
require_fixed README.md "Windows is not supported"
require_fixed README.md "docs/INSTALL.md"
forbid_fixed README.md "macOS x86_64"
# Version literals in the README rot; forbid the one that already did
# (the README pinned v0.0.3 while the repo shipped v0.7.0).
forbid_fixed README.md "v0.0.3"

require_fixed docs/INSTALL.md "Homebrew is the recommended install on supported macOS and Linux"
require_fixed docs/INSTALL.md "Supported install channels"
require_fixed docs/INSTALL.md "Homebrew"
require_fixed docs/INSTALL.md "Curl installer"
require_fixed docs/INSTALL.md "Release tarball"
require_fixed docs/INSTALL.md "From source"
require_fixed docs/INSTALL.md "nix develop -c cargo install --locked --path crates/phux"
require_fixed docs/INSTALL.md "nix develop -c cargo install --locked --path crates/phux-mcp"
require_fixed docs/INSTALL.md 'Every portable tarball and installer path includes `phux-mcp`'
# Backticked in the doc since the curated-docs truth pass (6968cf06); match the
# rendered claim, not the old unformatted spelling.
require_fixed docs/INSTALL.md '`cargo install phux` is unsupported'
# The seeded-v0.0.1 caveat used to be asserted against docs/INSTALL.md. That
# truth pass dropped it there — reasonably, since v0.0.1 is ten minor versions
# stale and INSTALL.md is the user-facing page — but the warning still matters
# to whoever points a tap or installer at a tag, so it is pinned where it now
# lives instead of being resurrected in INSTALL.md.
require_fixed docs/RELEASING.md 'do not point installers or the tap at it'
require_fixed docs/INSTALL.md "Windows is not supported"
require_fixed docs/INSTALL.md "First run: persistent session + agent loop"
require_fixed docs/INSTALL.md 'verifies the release `.sha256` sidecar before unpacking'
require_fixed docs/INSTALL.md "| macOS (x86_64) | Not supported. No official release artifact; Homebrew and the curl installer both refuse. Source: yes. |"
require_fixed docs/INSTALL.md "| Linux aarch64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |"

require_fixed docs/RELEASING.md "phux and phux-mcp artifacts"
require_fixed docs/RELEASING.md "cargo install phux is unsupported"
require_fixed docs/RELEASING.md "Windows is not supported"
require_fixed docs/RELEASING.md "Release cockpit"
require_fixed docs/RELEASING.md "just release-preflight vX.Y.Z"
require_fixed docs/RELEASING.md "CARGO_REGISTRY_TOKEN"
require_fixed docs/RELEASING.md "portable public release"
require_fixed docs/RELEASING.md 'explicit `v0.0.1` refusal'
require_fixed docs/RELEASING.md "aarch64-apple-darwin"
require_fixed docs/RELEASING.md "x86_64-unknown-linux-gnu"
require_fixed docs/RELEASING.md "aarch64-unknown-linux-gnu"
# The release flow is release-please-driven. The docs must describe THAT flow,
# not the retired hand-typed-tag dispatch.
require_fixed docs/RELEASING.md "release-please"
require_fixed docs/RELEASING.md "Mark the open **release-please** PR"
require_fixed docs/RELEASING.md "then merge it"
require_fixed docs/RELEASING.md "publish-crate.yml"
# The old manual cockpit is gone; catch a doc that drifts back to it.
forbid_fixed docs/RELEASING.md "publish_protocol"
forbid_fixed docs/RELEASING.md "crates_io_confirm"

require_fixed scripts/install.sh 'macOS x86_64 has no official release artifact; use a source build'
require_fixed scripts/install.sh 'download "$sha_url" "$sha_path"'
require_fixed scripts/install.sh 'sha256sum -c "$(basename "$sha_path")"'
require_fixed scripts/install.sh 'shasum -a 256 -c "$(basename "$sha_path")"'
require_fixed scripts/install.sh '"${stage_name}/phux-mcp"'
require_fixed scripts/install.sh 'cp -f "${stage_dir}/phux-mcp" "${install_dir}/phux-mcp"'

require_fixed justfile "release-preflight TAG:"
require_fixed justfile "release-preflight-fast TAG:"
require_fixed scripts/release-preflight.sh "cargo publish --dry-run --allow-dirty -p phux-protocol"

require_fixed scripts/gen-formula.sh 'bin.install "phux-mcp"'
require_fixed scripts/gen-formula.sh 'assert_path_exists bin/"phux-mcp"'
require_fixed scripts/gen-formula.sh 'strategy :github_latest'
require_fixed scripts/gen-formula.sh 'assert_match version.to_s, shell_output("#{bin}/phux --version 2>&1")'
# A platform with no on_* override silently falls back to the formula's
# top-level url. macOS ships arm64 only, so the generator must emit a fatal
# arch guard or Intel Macs install an arm64 binary that cannot exec.
require_fixed scripts/gen-formula.sh 'depends_on arch: :arm64'
# The generator must not know about targets the release matrix never builds.
forbid_fixed scripts/gen-formula.sh 'x86_64-apple-darwin'

require_fixed .github/workflows/release.yml 'cargo +1.90.0 build --release --bin phux --bin phux-mcp'
require_fixed .github/workflows/release.yml 'cp -f target/release/phux target/release/phux-mcp'
require_fixed .github/workflows/release.yml 'target: aarch64-apple-darwin'
require_fixed .github/workflows/release.yml 'target: x86_64-unknown-linux-gnu'
require_fixed .github/workflows/release.yml 'target: aarch64-unknown-linux-gnu'
# Intel macOS is deliberately unbuilt: the free macos-13 runner is retired and
# the surviving Intel images are `-large` class, which GitHub bills even on
# public repos. If this target is ever added, the guards above must come out
# together with it.
forbid_fixed .github/workflows/release.yml 'target: x86_64-apple-darwin'
require_fixed .github/workflows/release.yml 'https://ziglang.org/download/${ZIG_VERSION}/${archive}'
# The link check must run on every matrix leg. It was macOS-only for its whole
# life, so the Linux artifacts shipped unchecked; pin both the call and the
# Linux half of the script it calls.
require_fixed .github/workflows/release.yml 'bash scripts/check-binary-portability.sh'
# The tap is a single moving pointer and this workflow is dispatchable against
# any tag, so backfilling an old release must not rewrite the formula backwards.
require_fixed .github/workflows/release.yml 'refusing to downgrade it to'
require_fixed scripts/check-binary-portability.sh 'check_elf'
require_fixed scripts/check-binary-portability.sh 'check_macho'
require_regex .github/workflows/release.yml 'test -x .*phux-mcp|command -v .*phux-mcp|./phux-mcp --'

forbid_fixed .github/workflows/release.yml 'mlugg/setup-zig'
forbid_fixed .github/workflows/release.yml 'actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5'
forbid_fixed .github/workflows/release.yml 'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02'
forbid_fixed .github/workflows/release.yml 'actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093'
forbid_fixed .github/workflows/release.yml 'softprops/action-gh-release@3bb12739c298aeb8a4eeaf626c5b8d85266b0e65'

# --- Atomic release boundary (release-please) ---------------------------------
#
# release-please owns the TAG and creates the RELEASE + BODY as a draft.
# release.yml owns the ASSETS and the one-way draft -> published transition;
# neither workflow may recreate the release or rewrite the changelog body.
require_fixed .github/workflows/release.yml 'gh release upload'
require_fixed .github/workflows/release.yml "needs.build.result == 'success'"
require_fixed .github/workflows/release.yml 'gh release edit "$TAG" --draft=false'
require_fixed release-please-config.json '"draft": true'
forbid_fixed .github/workflows/release.yml 'softprops/action-gh-release'
forbid_fixed .github/workflows/release.yml 'generate_release_notes'
# release.yml is called by release-please and must stay dispatch/call-only. It
# must never create a tag, and must never publish to crates.io: an irreversible
# publish has no human in an automated path. publish-crate.yml is that path.
forbid_fixed .github/workflows/release.yml 'publish_protocol'
forbid_fixed .github/workflows/release.yml 'crates_io_confirm'
forbid_fixed .github/workflows/release.yml 'cargo publish'

require_fixed .github/workflows/release-please.yml 'googleapis/release-please-action'
require_fixed .github/workflows/release-please.yml 'uses: ./.github/workflows/release.yml'
# release-please cannot update Cargo.lock; cargo re-resolves it on the PR branch.
require_fixed .github/workflows/release-please.yml 'cargo +1.90.0 update --workspace'

# `release-type: rust` is fatally broken on this repo: its CargoToml updater
# throws on our virtual workspace root (no [package] section) and on every
# member crate (`version.workspace = true` is a table, not a tagged scalar).
# The root Cargo.toml is bumped by a generic TOML jsonpath updater instead.
require_fixed release-please-config.json '"release-type": "simple"'
forbid_fixed release-please-config.json '"release-type": "rust"'
require_fixed release-please-config.json '"jsonpath": "$.workspace.package.version"'
# Without this, the first `feat!:` bumps 0.x straight to 1.0.0.
require_fixed release-please-config.json '"bump-minor-pre-major": true'

# TAG SHAPE. release-please defaults `include-component-in-tag` to TRUE, and with
# `"package-name": "phux"` that renders the tag as `phux-v0.2.0`. Every downstream
# consumer of the tag assumes a bare `vX.Y.Z`: release.yml's `^v[0-9]+...` regex,
# check-release-version.sh, install.sh's `case "$version" in v*)` guard, and
# gen-formula.sh's artifact URLs. A component-prefixed tag would sail past all of
# them and publish a release with zero attached artifacts.
require_fixed release-please-config.json '"include-component-in-tag": false'

# The release PR is opened, and the Cargo.lock sync is pushed, as a GitHub App —
# NOT GITHUB_TOKEN. main's ruleset requires the `check`/`test` contexts with an
# empty bypass list, and GitHub raises no workflow runs for GITHUB_TOKEN events,
# so a GITHUB_TOKEN-authored release PR is unmergeable by anyone. See the header
# of release-please.yml.
require_fixed .github/workflows/release-please.yml 'actions/create-github-app-token'
forbid_fixed .github/workflows/release-please.yml 'token: ${{ secrets.GITHUB_TOKEN }}'

require_fixed .github/workflows/publish-crate.yml 'workflow_dispatch'
require_fixed .github/workflows/publish-crate.yml 'environment: crates-io'

# --- Self-update contract (ADR-0074) ------------------------------------------
#
# `phux update` derives its download URLs and its archive-member allowlist from
# release.yml's packaging step. That makes the artifact naming a consumed
# contract, not a convention: rename an artifact or drop the sidecar and every
# already-installed phux loses the ability to update itself. The pins below tie
# the three halves together — what the workflow writes, what the code expects,
# and what the docs promise — so a change to any one of them fails here rather
# than at a user's terminal.
require_fixed .github/workflows/release.yml 'stage="phux-${tag}-${target}"'
require_fixed .github/workflows/release.yml 'echo "${sha}  ${stage}.tar.gz" > "${stage}.tar.gz.sha256"'
require_fixed crates/phux/src/commands/update/release.rs 'format!("phux-{tag}-{target}")'
require_fixed crates/phux/src/commands/update/release.rs 'releases/download/{tag}/{archive}'
require_fixed crates/phux/src/commands/update/release.rs 'format!("{archive_url}.sha256")'
# The verification must precede the unpack; these two are the load-bearing
# functions and their order is asserted by the unit tests next to them.
require_fixed crates/phux/src/commands/update/apply.rs 'pub(crate) fn verify_archive'
require_fixed crates/phux/src/commands/update/apply.rs 'pub(crate) fn unpack_verified'
# Installs phux does not own are never mutated.
require_fixed docs/INSTALL.md '## Updating'
require_fixed docs/INSTALL.md 'brew upgrade phall1/tap/phux'
require_fixed docs/INSTALL.md 'nix profile upgrade phux'
require_fixed docs/INSTALL.md 'nixos-rebuild switch'
require_fixed docs/INSTALL.md 'Verifies the checksum before unpacking anything'
require_fixed docs/INSTALL.md 'phux update --rollback'
require_fixed docs/RELEASING.md 'This layout is a consumed contract'

if [ "$failures" -ne 0 ]; then
  printf 'install surface check failed: %d missing contract item(s)\n' "$failures" >&2
  exit 1
fi

echo "install surface check passed"
