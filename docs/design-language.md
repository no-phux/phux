# Design language

One schema, one house look, and derived themes. Every phux client (the
desktop and phone app on Rune, the TUI, phux-web, phux-mobile and the
website) draws its chrome from the same tokens, authored in this repository.
This page states the contract. The values live in the tokens; no number is
restated here. Artwork (icons, logos, the fox) is a separate
[brand asset](./brand-assets.md).

## Where the tokens are

The sources are `crates/phux-design/tokens/src`: the house look
(`house.json`), the solver constants (`derive.json`), the theme catalog
(`themes.json`), the reader and forced-colours variants, the status ladder,
the vocabulary, identity colours, the closed schema and `version.txt`.
`just design-gen` (`bun tools/main.ts design-gen`, in `tools/src/design`)
fits, solves and verifies them and writes every export; the `guards` lane
runs it with `--check`, so a hand edit or a stale export fails CI.

| Export | Read by |
| --- | --- |
| `crates/phux-design/src/generated/tokens.rs` | `phux_design::tokens` |
| `crates/phux-design/tokens/themes.json` | `phux_design::catalog()` |
| `crates/phux-design/tokens/derive-vectors.subset.json` | `tests/vectors.rs`, Rune's tests |
| `clients/phux-web/assets/phux-tokens.css` | phux-web, the website (also a release asset) |
| `clients/rune/ios/Assets/AccentColor.colorset/Contents.json` | the Rune iOS shell |

The mobile exports (Swift and Kotlin tokens and themes, Android resources,
string catalogs and iOS colour sets) are not committed. `design-gen
--mobile-out DIR` writes them, and both mobile FFI archives carry them under
`design/` with a `manifest.json` of their sha256 and the phux commit. The
JSON exports are marked `linguist-generated` in `.gitattributes`, and the
code exports carry an `@generated` header.

## The house look and derived themes

The generator holds the house look (and its increased-contrast and reader
variants) to the floor checks in both modes, and refuses to write exports
when one fails.
With no theme picked, a client draws the house look in the system
appearance, and the house terminal palette ships as the catalog theme `phux`
(`phux-light`).

A theme the person picks is *derived*: `phux_design::derive` turns its
terminal palette (background, foreground, the 16 ANSI colours, an optional
accent and selection) into every role. The solver keeps the ground, steps
the layers toward the ink, and then moves each word-carrying ink toward
white or black only as far as it must to read on every surface it can land
on, washes included. A derived theme that still falls short of a floor
ships anyway, because the person chose it.

The solver exists in the generator (TypeScript) and in `phux-design`
(Rust), and both match the golden vectors exactly: colours as `#RRGGBBAA`
strings, contrast within 1e-12. Luminance goes through the shipped table,
never `pow`, and arithmetic is IEEE-754 binary64 with no fused multiply-add.
The generator's tests run the full vector set and the floor checks.
`crates/phux-design/tests/vectors.rs` checks the Rust port against the
generated subset (`tokens/derive-vectors.subset.json`), so a drift fails
here too. Mobile calls the Rust solver through `phux-client-ffi` rather than
porting it.

## Roles

Clients name roles, never colours. The groups:

- `layer`: `ground`, `sidebar`, `raised`, `overlay` (opaque, back to front)
  and the one `scrim`.
- `text`: `text`, `dim`, `faint`, `inverse`. Text is never dimmed with
  opacity, and `faint` never sits on a wash.
- `border`: translucent `muted`, `base`, `strong`; opaque `control` (field
  and toggle outlines) and `focus` (the keyboard ring).
- `wash`: `hover`, `press` (which replaces hover), `current`, `selected`.
- `accent`: the one primary fill per view, its ink, and accent as text.
- `status`: the seven tones, each with a `_wash`, `_edge` and `_glow`
  companion whose alpha is in the colour.
- `match`, `diff`, `syntax`: search hits, diffs and code, which never borrow
  a status tone.
- `selection`, and `flat`: each translucent role composited over each layer,
  for renderers that cannot blend.

Colour means state or identity, nothing else: accent marks focus and the
primary action, tones mark agent state, host and session colours mark
identity.

## Status

Status is colour, word and mark together, so it reads in monochrome too.
`phux_design::tone_of` is the one mapping from pane facts (agent state,
attention, exit code, transport, crash, visited) to a ladder state. The
ladder, its marks, ASCII fallbacks and ranks are `tokens::STATUS`; the words
and ICU plurals are `tokens::VOCABULARY` (`phux_design::word`,
`phux_design::plural`). The canonical word for an agent waiting on you is
"needs you". GUI clients draw the marks as vector icons; the TUI and the CLI
use the Unicode glyphs, or the ASCII column for the whole ladder in a CJK or
non-UTF-8 locale.

## Everything else

Type (seven roles per form factor), radius, space, icon sizes, layout,
haptics, elevation (tier stacks of at most four shadow layers, glow the only
coloured one), motion (spring pairs, curves and loops) and materials (glass
and metal) are token groups too. Off-scale values are bugs. Each platform
has a raw-style-literal check whose baseline only shrinks.

Accessibility modes are part of the schema: increased contrast (a solved
variant with higher floors), forced colours, reduce transparency, reduce
motion, dynamic type, localisation and right-to-left layout, flash safety
and touch targets.

## Changing the design

Edit `crates/phux-design/tokens/src` and run `just design-gen`; when a
colour misses a floor, `bun tools/main.ts design-gen --suggest` prints the
least move that clears it. Never edit a generated file. A role a client
needs and the schema lacks is a schema change, not a local colour. Bump
`version.txt` when the exports change.
