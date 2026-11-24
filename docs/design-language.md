---
group: internals
order: 5
---

# Design language

The contract every phux client (Rune, the TUI, phux-web, mobile and the
website) follows for colour, type, status and artwork. Values live in the
tokens and the brand bundle; none are restated here.

## Tokens

Sources are in `crates/phux-design/tokens/src`: the house look, solver
constants, theme catalog, variants, status ladder, vocabulary, identity
colours, the closed schema and `version.txt`. The generator in
`tools/src/design` fits, solves and verifies them and writes every export.

```sh
just design-gen                                  # regenerate exports
bun tools/main.ts design-gen --check             # CI: refuse stale or hand-edited exports
bun tools/main.ts design-gen --suggest           # least move that clears a failing floor
bun tools/main.ts design-gen --mobile-out DIR    # also write mobile exports and manifest.json
```

Never edit a generated file. A role a client needs and the schema lacks is a
schema change, not a local colour. Bump `version.txt` when exports change.
Mobile exports are not committed; the mobile FFI archives carry them.

## House look and derived themes

The house look and its variants must pass the contrast floors in both modes,
or the generator writes nothing. With no theme picked, a client draws the
house look in the system appearance.

A theme the person picks is derived: `phux_design::derive` turns its terminal
palette into every role, keeping the ground and moving each ink only as far as
it must to read on every surface it can land on. A derived theme that still
misses a floor ships anyway, because the person chose it.

The solver exists in TypeScript (generator) and Rust (`phux-design`), and both
must match the golden vectors exactly. Luminance uses the shipped table, never
`pow`, in IEEE-754 binary64 without fused multiply-add.
`crates/phux-design/tests/vectors.rs` checks the Rust port against
`tokens/derive-vectors.subset.json`. Mobile calls the Rust solver through
`phux-client-ffi` rather than porting it.

## Rules

- Clients name roles, never colours.
- Colour means state or identity, nothing else: accent marks focus and the
  primary action, tones mark agent state, host and session colours mark
  identity.
- Text is never dimmed with opacity, and `faint` never sits on a wash.
- One accent fill per view: the primary action. `press` replaces `hover`.
  Search, diff and syntax colours never borrow a status tone.
- Status is colour, word and mark together, so it reads in monochrome.
  `phux_design::tone_of` is the one mapping from pane facts to a status;
  `phux_design::word` and `plural` give its words. GUI clients draw marks as
  icons; the TUI and CLI use glyphs, or the ASCII column in a CJK or
  non-UTF-8 locale.
- Type, radius, space, elevation, motion and materials are token groups.
  Off-scale values are bugs; Rune and web have a raw-style-literal check whose baseline only shrinks.
- Accessibility modes (increased contrast, forced colours, reduced
  transparency and motion, dynamic type, right-to-left) are part of the schema.

## Brand assets

Artwork (icons, logos, the fox) is not tokens. It comes from the private
`no-phux/phux-brand` library as versioned, approved export bundles; the public
kit is at [phux.sh/brand](https://phux.sh/brand).

`brand.config.json` maps bundle exports to local paths; `brand.lock.json` pins
the version, source revision, archive SHA-256 and per-file checksums. Builds
verify committed bytes without private credentials.

```sh
just brand-check                          # verify committed assets against the pin
just brand-sync /path/to/bundle.zip SHA256  # install an approved bundle (or roll back)
just brand-restore                        # repair local drift from the public archive
```

- Change artwork in phux-brand and publish a new version; never edit installed
  bytes or checksums here. Product code owns behavior, animation and platform
  packaging.
- The hourly `brand-update` workflow opens an ordinary PR when a new version
  is released. It never downgrades unless dispatched with an explicit
  `version` and `allow_rollback=true`:

  ```sh
  gh workflow run brand-update.yml --repo no-phux/phux
  ```

- A rollback bundle must supply every export the mapping requires; revert
  matching config and platform code with the pin. Never replace a published
  brand version.
- The wordmark is artwork, not a font: do not recreate it with a monospace
  face. `docs/assets/logo-light.svg` and `logo-dark.svg` back the README's
  `picture`; keep their clearspace and alternative text.
- `clients/rune/packaging/AppIcon.{svg,png}` is the selected app icon, not a
  wordmark. CLI, TUI, JSON and diagnostics output stay plain text: no splash or
  terminal art.
