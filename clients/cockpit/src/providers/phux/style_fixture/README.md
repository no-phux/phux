---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-07
---

# Remote text style projection fixture

**TL;DR.** These frames are encoded by Phux's Rust wire codec, fed to the
real C client in `presentation_tests.zig`, and projected into owned canvas
cells. They establish structural style and lifetime correctness, not actual
AppKit/CoreText raster fidelity.

Regenerate from the repository root:

```sh
CARGO_TARGET_DIR="$PWD/target" cargo run --locked -p phux-client-ffi \
  --example cockpit_style_fixture --profile ffi-dev
```

The generator is
[`cockpit_style_fixture.rs`](../../../../../../crates/phux-client-ffi/examples/cockpit_style_fixture.rs).
It encodes HELLO_OK, ATTACHED, synthesized-VT bootstrap begin/chunk/ready and
ATTACH_READY. The Zig test invokes the public C API, obtains an actual dense
grid from libghostty, copies it, destroys the C client, and then asserts on the
owned cells. The separate admission test mutates synthetic C spans to exercise
invalid offsets, lengths, UTF-8 and arena budgets before replacing valid state.

## Source contracts

- `crates/phux-client-ffi/src/client.rs::push_flattened_cell` resolves palette
  entries to RGB but leaves inverse, faint and invisible as flags. The adapter
  swaps inverse exactly once. Its legacy v1 copy uses per-cell background
  for faint; metadata-aware projection supplies the terminal default instead.
  Invisible zeroes the glyph while retaining decorations.
- The v1 grid alone has no palette-index or underline-color provenance.
  Its legacy copy preserves the supplied RGB. The additive metadata companion
  now supplies this provenance to the shipping `copyClient` path; see
  [Remote grid metadata](../../../../docs/REMOTE_GRID_METADATA.md).
- Native SDK `34cc9d5571599d5ea4feafc9260f36575e67e77b` defines
  `canvas.terminal_grid.TerminalUnderline`. Named C values map to its named
  styles; no numeric enum casts or new style constants are involved.
- That SDK's `TerminalCell` has no hyperlink, faint, inverse, invisible or
  blink field. Glyph/color decisions are resolved in the producer. Explicit
  OSC 8 URIs are retained in `CanvasStore`'s owned, row-major sidecar and read
  through `hyperlinkAt`; this is storage, not a claim of UI link activation.
  Text and URI arenas each admit at most `max_grid_utf8_bytes` bytes. URI
  admission uses `PHUX_CLIENT_CELL_HYPERLINK`, independently of visible text.

## Original style-fix complexity evidence

This table records `27738342`; the metadata follow-up's measurements are in
[Remote grid metadata](../../../../docs/REMOTE_GRID_METADATA.md#review-and-complexity).

No project Zig CC tool/threshold is configured. Counts below use one plus
`if`, loops, `catch`, boolean `and`/`or`, and non-default switch arms; comments
and strings are excluded. Grouped switch labels count as one arm.

| Function | Before | After |
|---|---:|---:|
| `CanvasStore.copyBorrowed` | 43 | 7 |
| `CanvasStore.reserve` / `deinit` | 1 | 1 |
| `firstCodepoint` | 2 | removed |
| `copyRows` | — | 4 |
| `hyperlinkAt` | — | 4 |
| `Span.slice` / `Span.validate` | — | 1 / 4 |
| `text` / `hyperlink` | — | 1 / 2 |
| `validateShape` / `validate` / `admit` | — | 8 / 4 / 2 |
| `cell` / `resolveColors` / `codepoint` | — | 3 / 3 / 5 |
| `width` / `underline` / `selection` | — | 3 / 6 / 5 |
| test `feed` / `fixtureClient` / `fixtureGrid` | — | 1 / 2 / 1 |
| test `expectColorAndOccupancy` | — | 2 |
| Rust fixture `main` / `write` | — | 1 / 1 |

The highest remaining count is the eight-decision shape/pointer admission
check. Style enum mapping has six paths because the C API exposes six values;
each arm is a direct named SDK value.

## Review disposition

An independent, read-only Claude Sonnet review checked the C producer, local
palette, pinned SDK and new tests. It confirmed the deep-copy lifetime and
admission arithmetic, and found an additional dropped ABI cursor shape:
`PHUX_CURSOR_BLOCK_HOLLOW`. The projection now maps it directly to the SDK's
`.block_hollow`, covered by a separate C-record contract test.

The color-provenance/default-color and cursor metadata gaps found in the
original review are addressed by the additive metadata work in Bead `phux-r3nj`.
In particular, equal RGB values cannot reveal whether SGR 58 was explicit, or
which ANSI index produced a bold color. The fixture includes indexed-bold and
inverse/default-underline cells to verify the values actually supplied by the
ABI are preserved. Metadata-aware admission now also validates visible cursor
coordinates before copying.

Recorded regressions live in `scripts/guards/remote-cell-styles.guard`,
`remote-cell-hyperlinks.guard`, and `remote-cursor-hollow.guard`. Their breaks
restore the corresponding omissions from the pre-fix projection. Each was
run through `guard-red-run.sh` against its named test and the Phux-enabled
graph, with this worktree's FFI archive and private Zig cache.
