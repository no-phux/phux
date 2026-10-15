#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

failures=0

require_absent() {
  local file="$1"
  if [[ -e "$ROOT/$file" ]]; then
    phux_error "retired surface: $file"
    failures=$((failures + 1))
  fi
}

require_fixed() {
  local file="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$ROOT/$file"; then
    phux_error "missing: $file: $needle"
    failures=$((failures + 1))
  fi
}

forbid_fixed() {
  local file="$1"
  local needle="$2"
  if grep -Fq -- "$needle" "$ROOT/$file"; then
    phux_error "stale claim: $file: $needle"
    failures=$((failures + 1))
  fi
}

forbid_regex() {
  local file="$1"
  local regex="$2"
  if grep -Eq -- "$regex" "$ROOT/$file"; then
    phux_error "stale claim: $file: $regex"
    failures=$((failures + 1))
  fi
}

require_regex() {
  local file="$1"
  local regex="$2"
  if ! grep -Eq -- "$regex" "$ROOT/$file"; then
    phux_error "missing regex: $file: $regex"
    failures=$((failures + 1))
  fi
}

# The tarball member list is owned by pack-release.sh. Workflows and dist.sh
# must call it rather than copying docs inline — that inline copy is how
# LICENSE-MIT/LICENSE-APACHE survived the relicense and broke curl | sh.
require_fixed .github/workflows/release.yml 'bash scripts/pack-release.sh'
require_fixed .github/workflows/next-release.yml 'bash scripts/pack-release.sh'
require_fixed scripts/dist.sh 'bash scripts/pack-release.sh'
forbid_fixed .github/workflows/release.yml 'LICENSE-MIT LICENSE-APACHE'
forbid_fixed .github/workflows/next-release.yml 'LICENSE-MIT LICENSE-APACHE'

# --- phux.sh serves the installers -------------------------------------------
#
# Installer behavior is proven by the executable tests at the bottom of this
# script, which CI runs under dash, and by shellcheck in sh mode. The site
# that publishes them lives in no-phux/website; its check-publish.sh owns the
# copy, the cache headers, and the deploy-side byte compare. What this repo
# still has to prove is that an installer change tells that repo to rebuild.
# Miss a path and phux.sh keeps serving the old script.
require_absent scripts/install-cockpit.sh
require_absent scripts/cockpit-self-update.sh
require_absent scripts/test-cockpit-self-update.sh
require_fixed .github/workflows/website-dispatch.yml '- "docs/**"'
require_fixed .github/workflows/website-dispatch.yml '- ".agents/skills/using-phux/**"'
require_fixed .github/workflows/website-dispatch.yml '- ".agents/skills/using-phux-mcp/**"'
require_fixed .github/workflows/website-dispatch.yml '- ".agents/skills/using-phux-tools/**"'
require_fixed .github/workflows/website-dispatch.yml '- "scripts/product-skills"'
require_fixed .github/workflows/website-dispatch.yml '- "scripts/install.sh"'
require_fixed .github/workflows/website-dispatch.yml '- "scripts/install-desktop.sh"'
require_fixed .github/workflows/website-dispatch.yml '- ".github/workflows/website-dispatch.yml"'
forbid_fixed .github/workflows/website-dispatch.yml 'install-cockpit'
require_fixed .github/workflows/website-dispatch.yml 'event_type=phux-content'
require_fixed .github/workflows/website-dispatch.yml 'repos/no-phux/website/dispatches'
require_fixed .github/workflows/website-dispatch.yml 'client_payload[sha]'
require_fixed .github/workflows/website-dispatch.yml 'repositories: website'
# A published release moves the landing badge even when the release commit
# does not match the path filter. GITHUB_TOKEN cannot raise that event into
# the other repo, so the workflow uses the release App.
require_fixed .github/workflows/website-dispatch.yml 'release:'
require_fixed .github/workflows/website-dispatch.yml 'types: [published]'

# Only the CLI stream may be GitHub's "Latest": the installer and `phux update`
# read the releases/latest redirect before falling back to the release list.
require_fixed .github/workflows/release.yml 'gh release edit "$TAG" --draft=false --latest'
require_fixed .github/workflows/agent-integration-release.yml '--draft=false --latest=false'
require_fixed .github/workflows/desktop-release.yml '--draft=false --latest=false'

# --- The repository is no-phux/phux -------------------------------------------
#
# It was renamed from phall1/phux. Every URL below kept working only because
# GitHub still answers the old name with a redirect, which is somebody else's
# decision to revoke. An installed phux resolves its own updates through these
# constants, so a dropped redirect would strand every existing install.
forbid_fixed crates/phux/src/commands/update/release.rs 'phall1/phux'
# The CLI next-channel pointer must be an asset named channel.json.
# gh's `file#label` syntax labels the asset;
# it does not rename it, so the pointer is written under its real basename.
require_fixed scripts/publish-next-channel.sh 'pointer_name="channel.json"'
require_fixed scripts/publish-next-channel.sh 'channel_json="$pointer_dir/$pointer_name"'
require_fixed scripts/publish-next-channel.sh 'gh release upload next "$channel_json" --clobber'
# A refused retarget (workflows:write, which GITHUB_TOKEN cannot hold) must
# not skip the pointer upload. Both the script and the desktop publish step
# warn and continue.
require_fixed scripts/publish-next-channel.sh 'could not retarget the next release to ${sha}; publishing the pointer anyway'
require_fixed .github/workflows/next-release.yml 'could not retarget the next release to ${SHA}; publishing the pointer anyway'
forbid_fixed scripts/publish-next-channel.sh 'channel_json#'
require_fixed .github/workflows/next-release.yml 'PHUX_BUILD_CHANNEL: next'
require_fixed crates/phux/src/commands/update/release.rs 'pub(crate) const REPO: &str = "no-phux/phux";'

require_fixed just/release.just "release-preflight TAG:"
require_fixed just/release.just "release-preflight-fast TAG:"
require_fixed just/build.just "cargo build --locked -p phux -p phux-mcp --release"
require_fixed just/release.just "cargo publish --locked --dry-run -p phux-protocol"
require_fixed just/release.just "cargo publish --locked -p phux-protocol"
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
require_fixed .github/workflows/release.yml '--bin-dir target/release'
require_fixed .github/workflows/release.yml 'target: aarch64-apple-darwin'
require_fixed .github/workflows/release.yml 'target: x86_64-unknown-linux-gnu'
require_fixed .github/workflows/release.yml 'target: aarch64-unknown-linux-gnu'
# Intel macOS is deliberately unbuilt: the free macos-13 runner is retired and
# the surviving Intel images are `-large` class, which GitHub bills even on
# public repos. If this target is ever added, the guards above must come out
# together with it.
forbid_fixed .github/workflows/release.yml 'target: x86_64-apple-darwin'
require_fixed .github/workflows/release.yml 'bash scripts/install-zig.sh'
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
# publish.yml calls release.yml. release.yml stays call-only. It
# must never create a tag, and must never publish to crates.io: an irreversible
# publish has no human in an automated path. publish-crate.yml is that path.
forbid_fixed .github/workflows/release.yml 'publish_protocol'
forbid_fixed .github/workflows/release.yml 'crates_io_confirm'
forbid_fixed .github/workflows/release.yml 'cargo publish'

require_fixed .github/workflows/release-please.yml 'googleapis/release-please-action'
require_fixed .github/workflows/publish.yml 'uses: ./.github/workflows/release.yml'
forbid_fixed .github/workflows/release-please.yml 'wait_validation.py'
forbid_fixed .github/workflows/release-please.yml 'uses: ./.github/workflows/release.yml'
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
# The native demo image lives in no-phux/website. A release PR must not grow
# an extra-files entry that points back at a Dockerfile in this repo.
forbid_fixed release-please-config.json 'docs/site/worker/Dockerfile'
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
# A force-pushed release head is mergeable until sync-lockfile runs, because
# main does not require checks on the latest push. Hold the PR as draft in the
# same job as the force-push, and restore it only after the lockfile push.
require_fixed .github/workflows/release-please.yml 'gh pr ready "$num" --repo "$GITHUB_REPOSITORY" --undo'
require_fixed .github/workflows/release-please.yml 'gh pr ready "$PR_NUMBER" --repo "$GITHUB_REPOSITORY"'

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
# prepack prints a colourised banner, so the parse died on an
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

if [ "$failures" -ne 0 ]; then
  phux_error "install surface check failed: $failures missing contract item(s)"
  exit 1
fi

bash "$ROOT/scripts/check-product-skills-export.sh"
bash "$ROOT/scripts/test-pack-release.sh"
bash "$ROOT/scripts/test-publish-next-channel.sh"
bash "$ROOT/scripts/test-install.sh"
bash "$ROOT/scripts/sync-install-resolver.sh" --check
bash "$ROOT/scripts/test-install-resolution.sh"
python3 "$ROOT/scripts/test-install-desktop.py"
phux_ok "install surface check passed"
