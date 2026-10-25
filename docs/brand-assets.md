# Brand assets

Approved artwork comes from the private `no-phux/phux-brand` library.
Desktop, mobile, and website consume versioned export bundles. The public
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

## Selecting the identity

The private library's `profiles.json.selected` is the single selection for
Flat Fox, Silver Fox or Classic Prompt. Run `just profile <id>` there to
render the chosen profile for every platform, then review and publish its kit.
Flat Fox is the default. `identity/profile.json` travels with the assets and
is installed as `clients/desktop/packaging/brand-profile.json` here.

Desktop packaging uses `desktopBrand()` for the selected profile and its
PNG/SVG paths. Mobile drawing and launch silhouettes use their native brand
interface over the same generated geometry and finish. Clients keep stable
asset paths and do not hard-code individual profile names.

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
