# dsr Release Fallback

## What this is

`dsr` ("doodlestein self-releaser", forked to
[phall1/doodlestein_self_releaser](https://github.com/phall1/doodlestein_self_releaser))
is an operator-invoked CLI tool for working around GitHub Actions billing and
queue throttling. When hosted Actions is backed up or over budget, `dsr`
replays a repo's existing `.github/workflows/cockpit-release.yml` locally on the
operator's machine, then uploads the resulting artifacts to the GitHub
Release via `gh release upload`.

It is **not** a persistent listener, runner, or CI replacement. It does
nothing until a human runs a `dsr` subcommand. Its job is narrow: get a
release's build artifacts published when hosted Actions can't do it in a
reasonable time.

## Registration lives outside this repo

`dsr` is installed on the operator's Mac, but the old standalone Cockpit
registration is retired and no current machine registration exists. Any new
machine-local registration must point at the Phux monorepo checkout,
`.github/workflows/cockpit-release.yml`, and the `darwin/arm64` target built
natively on that same Mac. This app is
macOS-only per `app.zon`'s `platforms = ["macos"]`, and `act` cannot run
macOS jobs at all, so there is no Linux/`act` leg to configure here — the
native-build path is the only one that exists.

This doc is purely informational and operational. It exists so anyone
reading this repo understands what the fallback path is and what it does and
doesn't cover, without needing to go spelunking in a machine-local config
directory.

## Usage

All commands are manual, run by an operator when `dsr check` (or direct
observation of a stuck Actions run) indicates throttling:

```sh
dsr check no-phux/phux
dsr build phux-cockpit --targets darwin/arm64
dsr release phux-cockpit --version cockpit-vX.Y.Z
dsr fallback phux-cockpit --version cockpit-vX.Y.Z
```

`dsr check` reports whether GitHub Actions currently looks throttled for
this repo. `dsr build` runs the local replay without publishing. `dsr
release`/`dsr fallback` carry a local build through to uploading assets
against an existing GitHub Release for the given tag.

## The signing caveat — read this before using the fallback for a real release

`cockpit-release.yml`'s single `macos` job conditionally imports Developer ID
signing secrets (`MACOS_CERTIFICATE`, `MACOS_CERTIFICATE_PASSWORD`,
`MACOS_SIGNING_IDENTITY`) and notarization secrets (`APPLE_NOTARY_KEY`,
`APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`). Both groups are declared
`required: false` at the `workflow_call` level, and both are all-or-nothing:
supply zero secrets and the job proceeds with an ad-hoc signed build; supply
a partial set and it hard-fails.

`dsr`'s local config for this repo **deliberately does not configure those
secrets**. This is an explicit decision, not an oversight: `dsr` is
build-only, and replaying `cockpit-release.yml` locally without those secrets
naturally falls through to the workflow's own existing ad-hoc-signed path —
exactly the behavior the workflow already has for a normal hosted run with
no signing secrets configured.

The practical consequence: **a local `dsr` fallback release produces an
unsigned/ad-hoc build, not the normal Developer-ID-signed-and-notarized
one.** A genuinely signed, notarized release still requires hosted GitHub
Actions, because that's where those secrets live, in GitHub's managed
secret store. `dsr` must never be handed Apple signing or notarization
secrets locally — keeping that material off this Mac's disk and keychain,
outside GitHub's managed secret store, is the point. (This mirrors the
reasoning behind ADR-0084 in the operator's home-ops repo, made for a
different, now-retired self-hosted-runner approach, for the same reason.)

If a real release needs to go out during an outage and Developer ID signing
matters for that release, the fallback is not a substitute for waiting on or
escalating hosted Actions. Without the tap credential it prepares a verified,
ad-hoc-signed draft; a credentialed recovery is still required to publish it.

## The Homebrew tap stop point

`HOMEBREW_TAP_TOKEN` is optional at the `workflow_call` boundary so a
keyless local replay can build and preserve its result. The workflow itself has
one explicit keyless stop:
`Require Homebrew tap token before publication`.

The order is deliberate:

1. build and package the app;
2. upload the exact asset set only when the draft has no assets; an exact
   existing set is preserved, while a partial or unexpected set fails before
   any mutation;
3. download the assets; compare them byte-for-byte with the local package only
   when this run uploaded them, then intrinsically verify the remote checksums,
   signature mode, archive, and disk image;
4. fail with `KEYLESS_RELEASE_STOP` when the tap token is absent;
5. when the token is present, check out `no-phux/homebrew-tap`, generate and
   validate the cask, hash that generated working-tree file, push the real tap
   update, fetch `origin/main`, and prove its cask blob equals the pre-push hash;
6. only after that remote equality proof, annotate and publish the draft.

Consequently, `dsr fallback phux-cockpit --version cockpit-vX.Y.Z` without the
tap token is expected to exit nonzero at `KEYLESS_RELEASE_STOP`. The verified
assets remain attached to the **draft** Release for a credentialed rerun, but
the Release is not published and the tap is not changed. Do not describe this
state as a published or partially published release.

After `HOMEBREW_TAP_TOKEN` is restored in the repository secrets, rerun the
release workflow for the same tag:

```sh
gh workflow run cockpit-release.yml \
  --repo no-phux/phux \
  --field tag=cockpit-vX.Y.Z
```

The rerun preserves an exact existing asset set and rejects a partial or
unexpected set before mutation. Because fresh ZIP, DMG, and signing output is
not byte-reproducible, it does not compare regenerated local bytes with those
pre-existing assets; it verifies the downloaded remote assets intrinsically.
Those verified remote files replace the regenerated package outputs before the
cask digest is computed. The workflow hashes the generated working-tree cask
before invoking the tap's commit helper, performs the real tap update, fetches
`origin/main`, and requires its cask blob to equal that pre-helper hash. The
draft is published only after that proof succeeds.

If a Release was published by some other recovery path but its tap entry is
stale, delegate cask recovery to the tap's real `Update packages` workflow:

```sh
gh workflow run update-packages.yml \
  --repo no-phux/homebrew-tap \
  --field tool=phux-cockpit
```

That external workflow resolves the published Release, recomputes the archive
digest, and invokes its checked-in
`.github/scripts/gen-phux-cockpit-cask.sh`; there is no Cockpit-local
`scripts/gen-formula.sh`. It also runs every fifteen minutes, so waiting for its
next scheduled run is the credential-free alternative to explicit dispatch.
