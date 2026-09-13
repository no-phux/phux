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

# Prose may wrap across lines without changing the install contract.
require_prose() {
  local file="$1"
  local needle="$2"
  if ! tr '\n' ' ' < "$ROOT/$file" | tr -s '[:space:]' | grep -F -- "$needle" > /dev/null; then
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

forbid_regex() {
  local file="$1"
  local regex="$2"
  if grep -Eq -- "$regex" "$ROOT/$file"; then
    printf 'stale claim: %s: %s\n' "$file" "$regex" >&2
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
require_fixed README.md "## Install"
require_fixed README.md "brew trust --tap no-phux/tap"
require_fixed README.md "brew install no-phux/tap/phux"
forbid_fixed README.md "brew install phall1/phux/phux"
forbid_fixed README.md "brew install phall1/tap/phux"
require_fixed README.md "macOS arm64, Linux x86_64, and Linux arm64"
require_prose README.md "Windows is not supported"
require_fixed README.md "docs/INSTALL.md"
forbid_fixed README.md "macOS x86_64"
# Version literals in the README rot; forbid the one that already did
# (the README pinned v0.0.3 while the repo shipped v0.7.0).
forbid_fixed README.md "v0.0.3"
# Same defect, second file: docs/RELEASING.md carried "v0.0.3 is the current
# portable public release" all the way to v0.19.0. Prose that names a current
# version is stale by the next release; point at "the latest release" instead.
forbid_fixed docs/RELEASING.md "v0.0.3"

require_prose docs/INSTALL.md "Homebrew is the recommended day-to-day path on supported macOS and Linux"
require_fixed docs/INSTALL.md "Supported install channels"
require_fixed docs/INSTALL.md "Homebrew"
require_fixed docs/INSTALL.md "brew trust --tap no-phux/tap"
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
# First-run and headless guidance live in their canonical guides.
require_fixed docs/INSTALL.md '[Quickstart](./QUICKSTART.md)'
require_fixed docs/INSTALL.md '[Agents](./consumers/agents.md)'
require_fixed docs/INSTALL.md 'verifies the release `.sha256` sidecar before unpacking'
require_fixed docs/INSTALL.md 'prints the exact command to run next'
require_fixed docs/INSTALL.md 'only when that directory is not already on `PATH`'
require_fixed docs/INSTALL.md "| macOS (x86_64) | Not supported. No official release artifact; Homebrew and the curl installer both refuse. Source: yes. |"
require_fixed docs/INSTALL.md "| Linux aarch64 | Curl/tarball: yes. Homebrew: yes where Linuxbrew supports the host. Source: yes. |"

require_fixed docs/RELEASING.md "phux and phux-mcp artifacts"
require_fixed docs/RELEASING.md "cargo install phux is unsupported"
require_fixed docs/RELEASING.md "Windows is not supported"
require_fixed docs/RELEASING.md "Cutting a Cockpit release"
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
require_fixed scripts/install.sh 'publish_dir="$(mktemp -d "${install_dir}/.phux-install.XXXXXX")"'
require_fixed scripts/install.sh 'rollback_publish'
require_fixed scripts/install.sh 'mv "${publish_dir}/phux-mcp" "${install_dir}/phux-mcp"'
require_fixed scripts/install.sh 'echo "next: phux"'
require_fixed scripts/install.sh '--channel'
require_fixed scripts/install.sh 'PHUX_CHANNEL'
require_fixed scripts/install.sh '.phux-channel'
require_fixed docs/RELEASING.md 'next channel'
require_fixed docs/adr/0113-next-release-channel.md '`phux update'
require_fixed scripts/install.sh 'PATH remedy: export PATH=%s:"$PATH"'
require_fixed scripts/install.sh 'found_command="$(command -v phux 2>/dev/null || true)"'
# Both standalone scripts embed the same bounded structural JSON resolver.
# The executable tests cover mixed streams, pagination and metadata filtering.
require_fixed scripts/install.sh 'resolve_latest_version v'
require_fixed scripts/install-cockpit.sh 'resolve_latest_version cockpit-v'
require_fixed scripts/test-install.sh 'installer transaction tests passed'

# --- The Cockpit installer ---------------------------------------------------
#
# scripts/install-cockpit.sh is the same POSIX-sh, verify-before-install
# discipline as scripts/install.sh, pointed at the cockpit-vX.Y.Z stream.
# It is served at /install-cockpit (+.sh), so every guard below has a twin in
# the CLI block above. macOS-only: refusing anywhere else is the install, not
# a fallback.
require_regex scripts/install-cockpit.sh '^#!/bin/sh$'
forbid_fixed scripts/install-cockpit.sh '#!/usr/bin/env bash'
forbid_regex scripts/install-cockpit.sh '^[[:space:]]*[^#[:space:]].*pipefail'
forbid_regex scripts/install-cockpit.sh '^[[:space:]]*[^#[:space:]].*%q'
require_fixed scripts/install-cockpit.sh 'shell_quote()'
require_fixed scripts/install-cockpit.sh 'phux-cockpit-${semver}-macos-arm64.zip'
require_fixed scripts/install-cockpit.sh 'no-phux/phux/releases/download/${version}'
require_fixed scripts/install-cockpit.sh 'cockpit-vX.Y.Z'
require_fixed scripts/install-cockpit.sh 'Phux Cockpit is macOS-only'
require_fixed scripts/install-cockpit.sh 'Phux Cockpit ships arm64 only'
require_fixed scripts/install-cockpit.sh 'com.apple.quarantine'
require_fixed scripts/install-cockpit.sh 'rollback_publish'
require_fixed scripts/install-cockpit.sh '.phux-cockpit-install.lock'
require_fixed scripts/install-cockpit.sh 'printf '\''next: open %s\n'\'' "$(shell_quote "$installed_app")"'
require_fixed scripts/test-install.sh 'cockpit installer transaction tests passed'
forbid_fixed scripts/test-install.sh 'bash "$ROOT/scripts/install-cockpit.sh"'

# --- The installer is POSIX sh, and phux.sh serves it -------------------------
#
# https://phux.sh/install returns scripts/install.sh byte for byte, and the
# documented command pipes it to `sh`. On Debian and Ubuntu that is dash, so a
# bashism is not a style question: it is an install that dies on a stranger's
# machine. Two killed it before this was pinned — `set -o pipefail`, which dash
# rejected before 0.5.12, and `printf %q`, which no dash has ever implemented.
require_regex scripts/install.sh '^#!/bin/sh$'
forbid_fixed scripts/install.sh '#!/usr/bin/env bash'
# Matched against code only. The script's own header names both bashisms so the
# next reader knows why they are banned; a guard that could not tell a comment
# from a command would forbid saying so.
forbid_regex scripts/install.sh '^[[:space:]]*[^#[:space:]].*pipefail'
forbid_regex scripts/install.sh '^[[:space:]]*[^#[:space:]].*%q'
# The POSIX stand-in for `printf %q` that replaced it.
require_fixed scripts/install.sh 'shell_quote()'
# The transaction tests must drive the installer through a POSIX shell, or the
# bashism guards above are the only thing standing between dash and a user.
require_fixed scripts/test-install.sh 'INSTALLER_SH="$(command -v dash || echo /bin/sh)"'
forbid_fixed scripts/test-install.sh 'bash "$ROOT/scripts/install.sh"'

# The site publishes the script from this repo instead of keeping a copy. Both
# halves are load-bearing: sync-docs.ts does the copying, and site-deploy.yml's
# path filter is what makes an installer change redeploy the site at all. Miss
# the second and phux.sh keeps serving the old script with nothing to show for
# it in any diff.
require_fixed docs/site/scripts/sync-docs.ts 'scripts/install.sh'
require_fixed docs/site/scripts/sync-docs.ts '"public/install", "public/install.sh"'
require_fixed docs/site/scripts/sync-docs.ts 'scripts/install-cockpit.sh'
require_fixed docs/site/scripts/sync-docs.ts '"public/install-cockpit", "public/install-cockpit.sh"'
require_fixed docs/site/scripts/sync-docs.ts 'must start with #!/bin/sh'
require_fixed .github/workflows/site-deploy.yml '- "scripts/install.sh"'
require_fixed .github/workflows/site-deploy.yml '- "scripts/install-cockpit.sh"'
# Both release streams move a landing badge now, so both must redeploy it.
require_fixed .github/workflows/site-deploy.yml "startsWith(github.event.release.tag_name, 'cockpit-v')"
# The route itself is unprovable before a deploy, so the deploy asserts it.
require_fixed .github/workflows/site-deploy.yml 'Verify the installers are served'
require_fixed .github/workflows/site-deploy.yml 'if [ "$shebang" != '"'"'#!/bin/sh'"'"' ]; then'
# A committed copy is a second source of truth waiting to go stale.
require_fixed docs/site/.gitignore 'public/install'
require_fixed docs/site/.gitignore 'public/install.sh'
require_fixed docs/site/.gitignore 'public/install-cockpit'
require_fixed docs/site/.gitignore 'public/install-cockpit.sh'
require_fixed docs/site/public/_headers '/install-cockpit'

require_fixed README.md 'curl -fsSL https://phux.sh/install | sh'
require_fixed README.md 'curl -fsSL https://phux.sh/install-cockpit | sh'
require_fixed docs/INSTALL.md 'curl -fsSL https://phux.sh/install | sh'
require_fixed docs/INSTALL.md 'curl -fsSL https://phux.sh/install-cockpit | sh'
require_fixed docs/INSTALL.md '## Cockpit (native macOS)'
require_fixed clients/cockpit/README.md 'curl -fsSL https://phux.sh/install-cockpit | sh'
require_fixed docs/site/DEPLOY.md '/install-cockpit'
require_fixed docs/RELEASING.md 'curl -fsSL https://phux.sh/install | sh'
require_fixed docs/RELEASING.md 'Curl installer contract'
# The cockpit tag shape and ZIP name are consumed by the installer and the
# site badge; RELEASING must say so where the release is cut.
require_fixed docs/RELEASING.md 'phux-cockpit-<semver>-macos-arm64.zip'
require_fixed docs/RELEASING.md 'scripts/install-cockpit.sh'

# --- The repository is no-phux/phux -------------------------------------------
#
# It was renamed from phall1/phux. Every URL below kept working only because
# GitHub still answers the old name with a redirect, which is somebody else's
# decision to revoke. An installed phux resolves its own updates through these
# constants, so a dropped redirect would strand every existing install.
forbid_fixed scripts/install.sh 'phall1/phux'
forbid_fixed scripts/install-cockpit.sh 'phall1/phux'
forbid_fixed docs/INSTALL.md 'phall1/phux'
forbid_fixed docs/RELEASING.md 'phall1/phux'
forbid_fixed crates/phux/src/commands/update/release.rs 'phall1/phux'
forbid_fixed docs/site/scripts/sync-docs.ts 'phall1/phux'
require_fixed scripts/install.sh 'https://github.com/no-phux/phux/releases/download/${release_tag}'
# The next-channel pointer must be an asset named channel.json. gh's
# `file#label` syntax labels the asset; it does not rename it.
require_fixed scripts/publish-next-channel.sh 'pointer_dir/channel.json'
require_fixed scripts/publish-next-channel.sh 'gh release upload next "$channel_json" --clobber'
forbid_fixed scripts/publish-next-channel.sh 'channel_json#channel.json'
require_fixed scripts/install-cockpit.sh 'https://github.com/no-phux/phux/releases/download/${version}'
require_fixed crates/phux/src/commands/update/release.rs 'pub(crate) const REPO: &str = "no-phux/phux";'

require_fixed justfile "release-preflight TAG:"
require_fixed justfile "release-preflight-fast TAG:"
require_fixed justfile "cargo build --locked -p phux -p phux-mcp --release"
require_fixed justfile "cargo publish --locked --dry-run -p phux-protocol"
require_fixed justfile "cargo publish --locked -p phux-protocol"
require_fixed scripts/release-preflight.sh "cargo publish --locked --dry-run --allow-dirty -p phux-protocol"
require_fixed scripts/check-release-version.sh "cargo metadata --locked --format-version 1 --no-deps"

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

# The release compiler comes from rust-toolchain.toml, never a hardcoded
# version: a bump that edits only the toml must move every release lane with
# it, and a hardcoded channel in a workflow rots exactly like a version in a
# README (it did: the toml and two workflows carried separate pins).
require_fixed .github/workflows/release.yml 'bash scripts/build-release-binaries.sh "${{ matrix.target }}"'
forbid_regex .github/workflows/release.yml 'cargo \+1\.[0-9]+\.[0-9]+'
forbid_fixed .github/workflows/release.yml 'toolchain install 1.'
require_fixed .github/workflows/release.yml 'rust-toolchain.toml'
require_fixed scripts/build-release-binaries.sh 'cargo build --locked --release --bin phux --bin phux-mcp'
require_fixed scripts/dist.sh '.phux-cpu-baseline'
require_fixed .github/workflows/release.yml 'cp -f target/release/phux target/release/phux-mcp'
require_fixed .github/workflows/release.yml 'target: aarch64-apple-darwin'
require_fixed .github/workflows/release.yml 'target: x86_64-unknown-linux-gnu'
require_fixed .github/workflows/release.yml 'target: aarch64-unknown-linux-gnu'
# Intel macOS is deliberately unbuilt: the free macos-13 runner is retired and
# the surviving Intel images are `-large` class, which GitHub bills even on
# public repos. If this target is ever added, the guards above must come out
# together with it.
forbid_fixed .github/workflows/release.yml 'target: x86_64-apple-darwin'
require_fixed .github/workflows/release.yml 'bash scripts/install-zig.sh'
require_fixed scripts/install-zig.sh 'https://ziglang.org/download/${ZIG_VERSION}/${archive}'
require_fixed scripts/lib/dev-toolchain.sh '.config/zig-toolchain.json'
# The link check must run on every matrix leg. It was macOS-only for its whole
# life, so the Linux artifacts shipped unchecked; pin both the call and the
# Linux half of the script it calls.
require_fixed .github/workflows/release.yml 'bash scripts/check-binary-portability.sh'
# The tap is a single moving pointer and this workflow is dispatchable against
# any tag, so backfilling an old release must not rewrite the formula backwards.
require_fixed .github/workflows/release.yml 'refusing to downgrade it to'
require_fixed scripts/check-binary-portability.sh 'check_elf'
require_fixed scripts/check-binary-portability.sh 'check_macho'
require_fixed scripts/check-binary-portability.sh 'x86-64-v[234]'
require_regex .github/workflows/release.yml 'test -x .*phux-mcp|command -v .*phux-mcp|./phux-mcp --'

forbid_fixed .github/workflows/release.yml 'mlugg/setup-zig'
# Remaining setup-zig callers must disable the action's Zig-cache post
# step: it keys each save on run_id, so restores never hit and every job
# writes up to 2 GiB (phux-6khi). Our actions/cache keys must stay
# reusable too — a commit-SHA suffix is the same single-use pattern.
require_fixed .github/workflows/cockpit-sdk-head.yml 'use-cache: false'
require_fixed .github/workflows/cockpit-release.yml 'use-cache: false'
forbid_regex .github/workflows/cockpit-ci.yml 'cockpit-zig-.*github\.sha'
forbid_regex .github/workflows/cockpit-sdk-head.yml 'cockpit-zig-.*github\.sha'
forbid_regex .github/workflows/cockpit-release.yml 'cockpit-zig-.*github\.sha'
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
# release-please cannot update Cargo.lock; cargo re-resolves it on the PR
# branch, against the toolchain rust-toolchain.toml pins — never a hardcoded
# channel (see the release.yml guard above for why).
require_fixed .github/workflows/release-please.yml 'cargo update --workspace'
forbid_regex .github/workflows/release-please.yml 'cargo \+1\.[0-9]+\.[0-9]+'
forbid_fixed .github/workflows/release-please.yml 'toolchain install .'

# `release-type: rust` is fatally broken on this repo: its CargoToml updater
# throws on our virtual workspace root (no [package] section) and on every
# member crate (`version.workspace = true` is a table, not a tagged scalar).
# The root Cargo.toml is bumped by a generic TOML jsonpath updater instead.
require_fixed release-please-config.json '"release-type": "simple"'
forbid_fixed release-please-config.json '"release-type": "rust"'
require_fixed release-please-config.json '"jsonpath": "$.workspace.package.version"'
# Without this, the first `feat!:` bumps 0.x straight to 1.0.0.
require_fixed release-please-config.json '"bump-minor-pre-major": true'

# TAG SHAPE. release-please defaults `include-component-in-tag` to TRUE, and a
# root component renders the tag as `phux-v0.2.0`. Every downstream consumer of
# the tag assumes a bare `vX.Y.Z`: release.yml's `^v[0-9]+...` regex,
# check-release-version.sh, install.sh's `case "$version" in v*)` guard, and
# gen-formula.sh's artifact URLs. A component-prefixed tag would sail past all of
# them and publish a release with zero attached artifacts.
require_fixed release-please-config.json '"include-component-in-tag": false'

# ROOT COMPONENT. `package-name` is a SECOND source for the component, and
# `getBranchComponent()` reads it without honouring `include-component-in-tag`.
# With `"package-name": "phux"` the branch component is "phux" while the release
# PR branch is `release-please--branches--main` (component undefined), and
# `buildRelease()`'s standalone path — taken whenever the merged PR body carries
# exactly ONE component-less release section, i.e. every root-only cycle —
# refuses to create the release. That is how v0.19.0 was silently skipped: green
# run, `releases_created: false`, no tag, no artifacts. The integrations packages
# carry their component under `component`, so nothing in this file needs
# `package-name`. See the header of .github/workflows/release-please.yml.
forbid_fixed release-please-config.json '"package-name"'

# The release PR is opened, and the Cargo.lock sync is pushed, as a GitHub App —
# NOT GITHUB_TOKEN. main's ruleset requires the `check`/`test` contexts with an
# empty bypass list, and GitHub raises no workflow runs for GITHUB_TOKEN events,
# so a GITHUB_TOKEN-authored release PR is unmergeable by anyone. See the header
# of release-please.yml.
require_fixed .github/workflows/release-please.yml 'actions/create-github-app-token'
forbid_fixed .github/workflows/release-please.yml 'token: ${{ secrets.GITHUB_TOKEN }}'

require_fixed .github/workflows/publish-crate.yml 'workflow_dispatch'
require_fixed .github/workflows/publish-crate.yml 'environment: crates-io'
require_fixed .github/workflows/publish-crate.yml 'cargo publish --locked --dry-run -p phux-protocol'
require_fixed .github/workflows/publish-crate.yml 'cargo publish --locked -p phux-protocol'

# --- Agent integration release lane -------------------------------------------
#
# This lane publishes integrations/* off `<component>-vX.Y.Z` tags. Its first
# four invocations all failed and left four permanent 0-asset drafts with
# nothing on npm, and none of it was noticed: unlike the root lane, a component
# release stuck in draft is indistinguishable from one nobody cut. Each pin
# below is one of those four defects. The full post-mortem is in the header of
# the workflow.

# The publish job intentionally has no checkout, so gh has no git remote to
# infer the repository from and every `gh release ...` call fails with
# "fatal: not a git repository". GH_REPO is what replaces the remote.
require_fixed .github/workflows/agent-integration-release.yml 'GH_REPO: ${{ github.repository }}'

# `npm pack` runs `prepack`, whose stdout shares the fd with the JSON. opencode's
# prepack is tsup, which prints a colourised banner, so the parse died on an
# escape sequence. Pack to a directory and glob; never read npm's stdout.
require_fixed .github/workflows/agent-integration-release.yml 'npm pack --pack-destination'
forbid_fixed .github/workflows/agent-integration-release.yml 'npm pack --json'

# A relative tarball path containing `/` and no leading `./` is parsed by npm as
# the GitHub shorthand `owner/repo`. Tarball arguments must be absolute.
forbid_fixed .github/workflows/agent-integration-release.yml 'npm publish --dry-run "release/'
forbid_fixed .github/workflows/agent-integration-release.yml 'echo "tarball=${tarballs[0]}"'

# npm trusted publishing removes the long-lived credential entirely. npm accepts
# OIDC from GitHub-hosted runners only, and requires package repository.url to
# match the workflow repository exactly. Pin all three parts so this cannot drift
# back to a secret or a Blacksmith/self-hosted publish job.
require_fixed .github/workflows/agent-integration-release.yml 'runs-on: ubuntu-latest'
require_fixed .github/workflows/agent-integration-release.yml 'expected_repository="https://github.com/${GITHUB_REPOSITORY}.git"'
# Provenance and public access are release contracts (npm trusted publishing
# attaches the repository's OIDC identity). Trailing flags (e.g. --no-audit,
# see .npmrc) are tolerated; their absence is not.
require_fixed .github/workflows/agent-integration-release.yml 'npm publish --provenance --access public'
forbid_fixed .github/workflows/agent-integration-release.yml 'NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}'

# `-type f -name a -o -name b` binds as `(-type f -a -name a) -o (-name b)`,
# silently dropping the type filter from the second branch.
forbid_fixed .github/workflows/agent-integration-release.yml "-type f -name '*.tgz' -o -name '*.tar.gz'"

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
# `phux update` resolves "latest" the same way the installer does, and the
# same multi-stream defect applied: the redirect follows whichever stream
# shipped newest. The tag must come from the filtered releases list unless the
# redirect already names a core tag.
require_fixed crates/phux/src/commands/update/release.rs 'latest_core_tag_from_list'
require_fixed crates/phux/src/commands/update/release.rs 'releases?per_page=30'
# The verification must precede the unpack; these two are the load-bearing
# functions and their order is asserted by the unit tests next to them.
require_fixed crates/phux/src/commands/update/apply.rs 'pub(crate) fn verify_archive'
require_fixed crates/phux/src/commands/update/apply.rs 'pub(crate) fn unpack_verified'
# Installs phux does not own are never mutated.
require_fixed docs/INSTALL.md '## Updating'
require_fixed docs/INSTALL.md 'brew upgrade no-phux/tap/phux'
forbid_fixed docs/INSTALL.md 'brew upgrade phall1/tap/phux'
require_fixed docs/INSTALL.md 'nix profile upgrade phux'
require_fixed docs/INSTALL.md 'nixos-rebuild switch'
require_fixed docs/INSTALL.md 'Verifies the checksum before unpacking anything'
require_fixed docs/INSTALL.md 'phux update --rollback'
require_fixed docs/INSTALL.md '--channel next'
require_fixed docs/INSTALL.md 'PHUX_CHANNEL=next'
require_fixed docs/INSTALL.md 'sh -s -- --channel next'
require_fixed docs/RELEASING.md 'This layout is a consumed contract'

if [ "$failures" -ne 0 ]; then
  printf 'install surface check failed: %d missing contract item(s)\n' "$failures" >&2
  exit 1
fi

bash "$ROOT/scripts/test-install.sh"
bash "$ROOT/scripts/sync-install-resolver.sh" --check
bash "$ROOT/scripts/test-install-resolution.sh"
echo "install surface check passed"
