---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0177 — Release checksums are minisign-signed

**TL;DR.** CI signs every release asset's `.sha256` sidecar with one minisign
key held as the `PHUX_MINISIGN_SECRET_KEY` Actions secret and publishes
`<asset>.sha256.minisig`. `phux update` and Desktop's updater embed the
public keys, verify the signature before they trust any digest, and refuse a
missing or foreign one; `phux update` also refuses an unpinned stable
downgrade. Rotation is a key id plus a pre-published next key.

Status: Accepted
Date: 2026-10-17
Supersedes in part: [ADR-0074](./0074-self-update-trust-boundary.md) decision 1, [ADR-0176](./0176-the-release-pipeline.md) decision 3

## Context

[ADR-0074](./0074-self-update-trust-boundary.md) states the limit of its
checksum: the digest and the archive come from the same host over the same
TLS channel, so a compromised release host publishes a matching pair.
[ADR-0176](./0176-the-release-pipeline.md) moved the anchor to the channel
manifest's digest, which the same host serves, and named signing as the one
place to add an independent root. A self-updater that replaces the binary a
user is running needs a check an attacker who can rewrite release assets
cannot pass.

## Decision

1. **The signed object is the sidecar.** For every published asset
   (`phux` tarballs and Desktop zips, stable and next), CI writes
   `<asset>.sha256.minisig`, a minisign signature over the exact bytes of
   `<asset>.sha256` (`<64 hex>  <asset>`). The sidecar names its asset, so a
   signed checksum for one release cannot vouch for another.
2. **One signing key, held by Actions.** The unencrypted minisign secret key
   is the repository secret `PHUX_MINISIGN_SECRET_KEY`.
   `scripts/ci/sign-release.sh` signs on a GitHub-hosted Ubuntu runner and
   verifies the result against the keys `scripts/install.sh` embeds, so a
   secret no shipped binary trusts fails the release, not the user. Stable
   CLI assets are signed in `release.yml`; stable Desktop in a Linux job of
   `desktop-release.yml` before the draft publishes; next builds in
   `next-release.yml`, where `release_manifest.py` refuses an unsigned build.
   The macOS build host never sees the key.
3. **Clients embed the public keys.** `RELEASE_KEYS` in
   `crates/phux-client-runtime/src/release/signature.rs`, which `phux update`
   and Desktop share, and in `scripts/install.sh` (a unit test keeps them
   equal) lists the current key, then the next:

   | role | key id | public key |
   |---|---|---|
   | current | `9F105950F46456C9` | `RWTJVmT0UFkQn2Zw1mG2cIcm5O/9kLuffefLpHw3vwhWAxgn/gcsoxiX` |
   | next | `B9E437599627DC6A` | `RWRq3CeWWTfkuYUoONcL0LsC/L3BYav/SIeHRPDMauxHAZnrUd+UoYU1` |

4. **`phux update` fails closed.** It downloads the archive, the sidecar,
   and the signature; verifies the signature with the key its id names
   (prehashed signatures only); requires the sidecar to name the derived
   asset; requires the manifest's digest, when the manifest names the build,
   to equal the signed one; and only then hashes the archive. A signature
   that is missing, malformed, made by an unknown key, or invalid refuses
   with `update_signature_invalid` and installs nothing. A `--version` pin
   is checked the same way, so it no longer trusts its sidecar unsigned.
   Without a pin, a stable manifest that names a release older than the
   installed one is refused with `update_downgrade_refused` before anything
   is fetched; a downgrade takes `--version`.
5. **Desktop verifies in-process; the installer verifies when it can.**
   Desktop's updater fetches the sidecar and its signature, verifies them
   with the shared check, and requires the signed digest to equal the
   manifest's before it runs the bundled installer with that digest, so a
   GUI launch whose `PATH` has no `minisign` still fails closed.
   `scripts/install.sh` checks the same signature (prehashed only) when
   `minisign` is on `PATH`, refusing on any failure, and otherwise says it
   checked only the SHA-256. `PHUX_REQUIRE_SIGNATURE=1` makes a missing
   `minisign` a refusal; any value other than `0` or `1` refuses. The
   fail-open default for an interactive `curl | sh` is an accepted
   exception to "fail closed": that script arrives from the same host as the
   assets, so a host able to forge assets can also strip the check, and
   requiring a tool most machines lack would block first installs for no
   gain. Every updater path verifies without depending on `PATH`.
6. **Rotation is a key id and a next key.** Each signature carries its
   key id, and a client accepts any key in `RELEASE_KEYS`. The next key's
   secret is generated with the current one and then held encrypted (a
   minisign password, or a password manager or hardware store) on no machine
   that holds the current key, so one compromised workstation cannot leak
   both. To rotate,
   set the Actions secret to the next key: every binary built since this
   ADR already trusts it, so nothing else has to change first. A later
   release then ships `RELEASE_KEYS = [new current, freshly generated
   next]`, updating this table and the installer. A binary that missed two
   rotations cannot verify and is reinstalled with the installer, which a
   user runs outside the update path. On a suspected leak of the current
   key, rotate at once; on a leak of the next key, ship a release that
   replaces it.

## Why

minisign is one small, audited format with a verify-only Rust crate that has
no dependencies, a CLI in every package manager, and no infrastructure.
Signing the sidecar rather than the archive keeps signatures tiny, lets the
existing digest check stay the byte gate, and reuses a file every release
already publishes, including Desktop's per-asset sidecar. A next key that
every client already trusts makes rotation a secret change instead of a
release that must reach users before the switch, which is the bootstrapping
problem a key baked into the binary being replaced would otherwise have.
Verifying in CI against the embedded keys turns a wrong secret into a red
job rather than a fleet that cannot update.

## Tradeoffs

The key is an Actions secret, so anyone who can run a workflow with secrets
on this repository can sign. That is a smaller set than anyone who can write
a release asset, and the repository's ruleset governs it, but it is not an
offline key. The channel manifest stays unsigned: a host that rewrites it can
name an older, genuinely signed release. `phux update` refuses that on stable
without a pin and Desktop offers only a newer stable build, but next has no
order between commits, so a rewritten `next.json` can still name an older
signed next build.
Releases published before this ADR have no signature, so a pin to one is
refused; the installer without `minisign` still installs it. `curl | sh`
without `minisign` gains nothing, and with it gains only as much as the
script it downloaded from the same host.

## Alternatives

**Sigstore cosign keyless.** No key to hold or rotate, but verification needs
the Fulcio and Rekor trust roots, network access to the transparency log or
a bundled TUF root, and a far larger verifier than a terminal multiplexer
should link. Rejected on client weight.

**Sign each archive instead of its sidecar.** Equivalent security, but the
signature check would stream every archive through BLAKE2b in addition to
SHA-256, and Desktop's installer would need a second download path.

**Sign the channel manifest.** Closes the rollback gap above, but the
manifest is regenerated from GitHub's asset records by several workflows,
and a pin would still need per-asset signatures. A manifest signature can be
added on top of this one without changing it.

**One key, rotated by shipping its replacement first.** Every rotation would
need a release that trusts both keys to reach users before the switch; a
pre-published next key gives the same overlap without the wait.
