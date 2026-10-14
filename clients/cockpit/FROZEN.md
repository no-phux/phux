# Cockpit is frozen

The Zig Cockpit is not maintained. The GPUI desktop (`clients/desktop`)
is the native client. This tree stays so a later sprint can ship the
app again without reconstructing it.

Last version in this tree: `clients/cockpit/version.txt` (0.37.4 when
frozen). Decision: `docs/adr/0167-cockpit-is-frozen.md`. Consumer
guide: `docs/consumers/cockpit.md`.

## What still works

- The source and `just cockpit-*` recipes.
- `scripts/install-cockpit.sh` and `https://phux.sh/install-cockpit`,
  which install the last published `cockpit-vX.Y.Z` release.
- `publish.yml` can still publish an existing `cockpit-vX.Y.Z` tag
  through `cockpit-release.yml`.
- `cockpit-ci.yml` and `cockpit-sdk-head.yml` remain and run only when
  someone dispatches them.

## What stopped

- Pull requests, pushes to `main`, and the merge queue do not run
  Cockpit CI.
- `next-release.yml` does not build or publish a Cockpit archive.
- release-please does not version `clients/cockpit`.

## Build it locally

From the repository root, on an Apple-silicon Mac:

```sh
just doctor cockpit
just cockpit-test
just cockpit-build
```

Do not copy the result over an installed `phux`, and do not aim it at
the production socket.

## Turn automatic CI back on

In `.github/workflows/cockpit-ci.yml`, replace the `on:` block with:

```yaml
on:
  push:
    branches: [main]
  pull_request:
    types: [opened, synchronize, reopened, ready_for_review]
  workflow_dispatch:
  merge_group:
```

In `scripts/ci/classify-changes-test.py`, put `cockpit-ci` back in the
workflows that must declare `merge_group` and must not use an outer
`paths:` filter, and delete the manual-only test added with the freeze.

## Turn next-channel builds back on

In `.github/workflows/next-release.yml`, replace the frozen gate:

```bash
echo "cockpit:" >&2
echo "  frozen by ADR-0167; not building" >&2
cockpit=false
```

with:

```bash
echo "cockpit:" >&2
cockpit="$(decide cockpit-channel.json cockpit_needed)"
```

## Turn release-please back on

Add this package back to `release-please-config.json`, and set the
`.release-please-manifest.json` key `clients/cockpit` to the version in
`clients/cockpit/version.txt`. Restore the package assertions in
`scripts/check-release-orchestration.mjs`.
`clients/cockpit/scripts/check-release-version.sh` allows that key to be
absent while the package is frozen, and requires it to match
`version.txt` once the key is restored.

```json
"clients/cockpit": {
  "release-type": "simple",
  "component": "cockpit",
  "include-component-in-tag": true,
  "include-v-in-tag": true,
  "tag-separator": "-",
  "changelog-path": "CHANGELOG.md",
  "extra-files": [
    { "type": "generic", "path": "app.zon" },
    { "type": "generic", "path": "build.zig.zon" }
  ]
}
```

Update the docs that call the client frozen before treating it as a
shipping product again.
