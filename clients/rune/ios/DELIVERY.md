# Scheduled iPhone delivery

The [upstream follower](../../../scripts/rune-upstream/README.md) can update
an explicitly authorized, already paired iPhone without TestFlight.
Its standing installation order lives in `STEERING.md`. Its private
configuration is `PHONE.json` in the follower's home, **outside Git and
outside the public website**:

```json
{
  "schema": 1,
  "enabled": true,
  "device_udid": "REGISTERED_PHONE_UDID",
  "profile": "/absolute/private/Rune.mobileprovision",
  "identity": "APPLE_DISTRIBUTION_CERTIFICATE_SHA1",
  "hosting_metadata": "/absolute/private/existing-hosting.json",
  "site_directory": "/absolute/existing-public-root/scheduled",
  "base_url": "https://private.example/rune-preview/scheduled/",
  "receipt": "/absolute/private/phone-installed.json"
}
```

Validate this as data, never shell-source it. Require `schema == 1`;
`enabled == false` disables delivery without signing, publishing or installing.
Require `enabled == true` and complete, well-formed fields before proceeding.
Reject malformed configuration and missing fields; never guess another device,
profile, signing identity, path or host.

Use owner-only permissions. `hosting_metadata` describes the already running
private HTTPS host; verify it answers before using it. Do not create another
daemon or reset shared Tailscale Serve state. `site_directory` must be a
dedicated child of its document root. Keep previous previews and URLs intact.
Profiles, signing data, receipts, app containers and logs are never web content.

## Reconcile before building or signing

Select the newest clean main revision that passes the exact `ci-green`
release gate, including full native qualification. PR results or an ancestor's
green run do not qualify a different candidate. Desktop publication does not
install the phone; a debug simulator artifact is not a device build.

Read the marketing version from that revision's Rune manifest and derive its
numeric build identity below. **Query the phone first**, before a costly
build or any signing/publication:

```sh
xcrun devicectl device info apps --device REGISTERED_PHONE_UDID \
  --bundle-id sh.phux.rune.ios --include-all-apps --timeout 30 \
  --json-output /absolute/private/apps-before.json
```

Compare the app's `version` and `bundleVersion` numerically with the candidate.
Stop if it already matches or is newer; never downgrade. In particular, a
locked signing keychain must not make an already-current phone falsely pending.
An unavailable query leaves the installed state unknown, even when an earlier
receipt exists. Signing an OTA fallback can still be useful, but does not prove
the phone needs or received it.

## Distinguish development builds

Same-version development updates need distinct `CFBundleVersion` values.
For main's commit count `n` from `git rev-list --count REVISION`, use these
three numeric components:

```python
build = f"{1 + n // 10000}.{(n // 100) % 100}.{n % 100}"
```

This increases along main and fits Apple's numeric build components. Build
that exact clean revision with `build.py --target device --release` in an
isolated worktree; verify `build.json`'s revision, target and release profile.
Stamp the derived build number and a `PhuxRuneSourceRevision` string containing
the full revision into the **unsigned** app's Info.plist, before calling
`package_ota.py`. Keep `CFBundleShortVersionString` unchanged. Record the source
revision and both versions in private evidence. Never modify the source tree
or a previously signed bundle.

## Sign, publish and install

Reuse the [existing packager](./README.md#direct-installation-without-testflight)
and authorized local identity/profile/device. All entitlement, expiration,
signature, platform and device-coverage checks remain mandatory.

Package using a fresh revision-prefixed immutable child URL under `base_url`,
then publish **only `site/`** at the matching child of `site_directory`.
Do not republish another preview's root. Verify the HTTPS page/manifest MIME
types and the **complete** downloaded IPA's SHA-256 against the signed local
artifact, not just a header or partial download.

On a retry, reuse an existing immutable artifact only after verifying its
exact source revision, marketing/build versions, signature, current profile
authorization/device coverage and full HTTPS digest again. Never overwrite
an immutable preview or treat its existence as successful delivery.

For a pending update, attempt an in-place install of the **signed** Payload
bundle through this device's existing pairing:

```sh
xcrun devicectl device install app --device REGISTERED_PHONE_UDID \
  '/absolute/scratch/rune-ota/Payload/Phux Rune.app' --timeout 120 \
  --json-output /absolute/private/install.json
xcrun devicectl device info apps --device REGISTERED_PHONE_UDID \
  --bundle-id sh.phux.rune.ios --include-all-apps --timeout 30 \
  --json-output /absolute/private/apps-after.json
```

Require the independent after-query's `version` **and** `bundleVersion` to match
the signed bundle before writing `receipt`. Record the source revision, both
versions, authorized device, query evidence and signed-artifact digest.
Installation-command success alone is not verification. Never uninstall to
update: app data and Keychain pairing must remain intact. Do not re-pair,
clear credentials, launch the user's app or manufacture a new device connection.

## Blockers are not success

The paired-device install can run unattended only while Apple/device trust,
signing access and connectivity permit it. Do not extract login passwords,
export keys, change Keychain security or remove consent to make it pass.
Report the exact blocker and keep delivery pending in Beads/journal for the
next tick. Preserve a verified private signed OTA fallback when signing
succeeds; opening that link and any Apple prompts still need on-phone approval.
OTA publication is **not a silent remote installation**. Installed-version
evidence is not launch, OS keyboard/IME, accessibility or physical-feel proof.
