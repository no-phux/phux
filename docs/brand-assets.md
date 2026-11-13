# Brand assets

Approved artwork comes from the private `no-phux/phux-brand` library.
This repository, mobile, and website consume versioned export bundles. The public
kit and retained releases are published at [phux.sh/brand](https://phux.sh/brand).

`brand.config.json` owns the mapping from public export paths to local asset
files. `brand.lock.json` pins their version, source revision, archive SHA-256,
download URL, and individual checksums. Normal builds verify committed bytes
without fetching private artwork or requiring private credentials.

```sh
just brand-check
just brand-sync /path/to/phux-brand.zip SHA256
just brand-restore
```

The canonical installer is itself a release export, delivered to
`scripts/brand.py` through the same pin. Change artwork or static geometry
in phux-brand, inspect its preview, and publish a new approved version.
Consumer CI rejects edits that drift from the pin. Product code continues
to own behavior, animation, and platform resource packaging.

## Wordmark surfaces

Brand release **1.5.0** supplies the original PHUX Grid wordmark. It is artwork,
not a body font or an app icon. Its original
grid-built letterforms belong in the brand library; do not recreate them with
a system monospace font or copy another project's glyphs. A wordmark update
does not select a different fox profile.

| Surface | Consumer | Contract |
| --- | --- | --- |
| Repository README | `docs/assets/logo-light.svg`, `docs/assets/logo-dark.svg` | Light ink on dark backgrounds; dark ink by default, with plain `phux` alternative text. |
| Compatibility documentation consumers | `docs/assets/logo.svg`, `docs/assets/logo.png` | Retained SVG/PNG wordmark paths, with light ink for dark backgrounds. |
| Rune app and window icons | `clients/rune/packaging/AppIcon.{svg,png}` | Selected profile icon, not a wordmark. |
| Rune onboarding | `clients/rune/src/desk/onboarding/fox.rs` | Fox illustrations and selected icon; keep these distinct from the wordmark. |
| Rune fleet heading | `clients/rune/src/ui/galaxy.rs` | `PHUX // FLEET` is an interface label, not a hand-built logo. |
| CLI help and diagnostics | `crates/phux/src/help.rs`, `crates/phux/src/output.rs` | Plain terminal text; no graphical or ASCII splash. |
| TUI and native terminal attachment | `crates/phux-tui` | Terminal content and chrome; no wordmark splash. |

The `logos/wordmark-light.svg` and `logos/wordmark-dark.svg` exports map to
`docs/assets/logo-light.svg` and `docs/assets/logo-dark.svg` through
`brand.config.json`. The README uses a `picture` with a dark-color-scheme
source and a dark-ink fallback image, so the mark stays visible on either
GitHub background. Both SVGs have a `0 0 34 14` view box (17:7 aspect ratio),
including two grid units of clearspace; preserve that space when embedding.
The existing `logos/docs-wordmark.svg` and `logos/docs-wordmark.png` exports
remain mapped to the compatibility paths above. Import an
approved wordmark revision with `brand-sync`; do not edit the installed asset
bytes or their checksums by hand. The SVG is self-contained artwork, so README
rendering does not depend on an installed font. Keep alternative text when
embedding it, and keep ordinary product names and command output as text.

No terminal-art export is required by the current CLI or TUI. If a future
surface needs one, approve a terminal-safe representation in the brand library
first and retain readable plain-text output for narrow or noninteractive
terminals. Do not add decorative output to JSON, protocol, or diagnostic
payloads as part of an artwork update.

## Choosing branding in the apps

In the mobile app, open **Settings → Appearance → Brand profile** to choose Flat
Fox, Silver Fox or Classic Prompt. All choices ship offline and are saved on the
device. Changing a host or account does not reset it. The picker reports icon or
preference-save failures. Phux Rune ships the selected default icon and has no
picker.

iOS uses alternate app icons and Android uses launcher aliases alongside the
selected in-app marks.

## Selecting the release default

The private library's `profiles.json.selected` sets the shared release default for
Flat Fox, Silver Fox or Classic Prompt. Run `just profile <id>` there to
render the chosen profile for every platform, then review and publish its kit.
Flat Fox is the default. Here the selected app icon lands in
`clients/rune/packaging/AppIcon.{png,svg}`, which Phux Rune's packager reads,
and the docs wordmark in `docs/assets`. Mobile drawing and launch silhouettes
use their native brand interface over the selected geometry and finish. Clients
keep stable asset paths and do not branch on profile names.

## Publishing and updates

phux-brand checks the catalog's explicit public approval, generated mobile
exports, and archive integrity before creating an immutable GitHub release.
Drafts, private originals, and unreviewed media never enter its export bundle.
The bundle includes a preview, usage guidance, a manifest, and approved assets.

The hourly `brand-update` workflow in this repository reads those private
releases through the existing release bot. A read-only token is scoped to
phux-brand; a second token may open update PRs in phux, phux-mobile, and
website. Neither key nor private originals are copied into consumer repos.
The updater verifies the GitHub asset digest, every file checksum, and the
tagged source revision before proposing changes. All three consumer gates
run without private credentials, including builds from public forks.

Run an update immediately with:

```sh
gh workflow run brand-update.yml --repo no-phux/phux
```

Each consumer receives an ordinary PR and remains subject to its existing
merge/release gates. The website update also retains the approved release
archive and publishes its manifest/checksum through the existing deployment.
Updating branding does not automatically release a desktop or mobile app.

## Rollback and repair

Install an earlier bundle with its original SHA-256 to roll back locally.
For coordinated rollback PRs, dispatch `brand-update` with an explicit
`version` and `allow_rollback=true`. Scheduled updates never downgrade a pin.
Old public archives remain available so older consumer versions can restore
their assets. Never rewrite or replace a published brand version.

A rollback bundle must supply every export required by the checked-in mapping,
and its geometry must match the platform code. When reversing a change that added
exports or changed the geometry API, revert the matching consumer configuration
and platform code together with the pin. Missing required exports are rejected
before any files are written.

`brand-restore` repairs local drift using the existing public pin. A malformed
bundle fails before writing files; a failed write restores previous bytes.
An interrupted update is detected by `brand-check` and can be repeated.
Locally edited obsolete assets cause an error instead of being deleted.
The updater reports failures per repository and can safely be rerun after
correcting access or an export mapping.

Windows applies the chosen artwork to running and newly opened window icons.
Explorer and pinned shortcuts retain the release icon. The Windows CI fixture
exercises all approved PNGs through the actual managed native adapter without
a WSL coordinator.
