---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-07
---

# Shipping navigation seam

**TL;DR.** The TypeScript core presents a four-row, revision-fenced native
catalog. The engine serves `cockpit.navigation` and handles additive intent
tags 12 and 13 before interpreting a legacy intent's window byte. Snapshot
byte 23 reports provider connectivity independently of engine readiness.

## Engine hook contract

`src/cockpit/native/ts_navigation.zig` exports:

```zig
pub fn encode(model: *const Model, revision: u64, request: []const u8,
              out: []u8) Error![]const u8;
pub fn resolve(model: *const Model, current_revision: u64,
               expected_revision: u64, index: u16) ?PaletteDestination;
pub fn connection(model: *const Model) Connection;
```

Both catalog functions are read-only. The engine must advance its revision
whenever catalog membership, order, placement, or session identity changes.
That includes provider catalog replacement and reconnect, even when the visible
tab topology stays the same. Selection must resolve and activate on the owning
thread without an intervening catalog mutation.

`ts_protocol.decodeNavigationIntent(payload)` recognizes the existing 12-byte
intent envelope with two additive tags:

| Tag | Meaning | Bytes 10-11 |
|---|---|---|
| 12 | Reconnect Phux through the existing connection lifecycle | unused; TS sends 0,255 |
| 13 | Activate the selected catalog destination | little-endian **u16 index** |

Bytes 2-9 remain the little-endian expected revision. Decode these tags before
legacy window adoption: the high byte of a catalog index is **not** a window.
Existing `IntentKind` and `NativeCommand` discriminants are unchanged.

On tag 13, call `resolve(model, engine.revision, intent.expected_revision,
intent.index)`. A null result is a refusal. A successful result is the existing
`PaletteDestination` union:

- `placed_terminal`: reveal its window and tab, focus that exact terminal ref,
  and bring the platform window forward;
- `available_terminal`: attach/place the exact remote ref in the adopted window;
- `session`: select that session ID through the provider's existing lifecycle.

The model's current focused pane is never a substitute for the returned ref.
New Terminal uses legacy tag 2 with window 255, preserving native event-window
adoption rather than forcing window zero.

## Read-only page wire

Host request name: `cockpit.navigation`. Core effect key: `cockpit-navigation`.
The host bridge needs a pending completion/buffer independent of its
`cockpit-snapshot` completion; differently keyed requests must not overwrite
each other. All errors use the request's error completion.

All multibyte integers are little-endian. The request is:

| Offset | Value |
|---|---|
| 0 | version 1 |
| 1 | kind 3 |
| 2 | expected revision, u64 |
| 10 | filtered offset, u16 |
| 12 | UTF-8 query length, u8, at most 64 |
| 13 | query bytes |

The response echoes the complete request, then appends `total:u16`, `count:u8`,
and `count` records of `index:u16, label_length:u8, label_bytes`. The index is
into the **unfiltered** catalog, not into this page or the filtered results.
It remains valid across query/page changes at the same engine revision.

The existing workspace projection supplies enumeration and matching: every
placed pane in every open window, every unplaced remote ref, then every remote
session. Display labels identify window/tab, provider, availability, or session.
Search uses the projection's full metadata rather than the compact strip label.
An inventory larger than u16 is an explicit error, never a silently shortened
result. Current bounded model/provider inventories fit comfortably.

Four 32pt rows leave room at the 420pt minimum for the 40pt search field,
32pt heading and paging controls, notice, token gaps, and panel/outer padding.
Previous/Next work by pointer; arrow navigation crosses page boundaries. The
query, offset, and revision are echoed so late responses cannot replace newer
results. Invalidation immediately withdraws clickable rows. Refresh is always
reachable, including an error or empty result.

## Snapshot and connection state

Snapshot byte 23, formerly reserved, carries `local=0`, `connecting=1`,
`connected=2`, or `offline=3`. Engine `status=READY` means its snapshot was
accepted; the visible `connectionStatus` is separately derived from the provider.
An unavailable engine withdraws the connectivity claim. An offline provider
exposes Reconnect in main and secondary window chrome.

The toolkit's 4096-byte limit is unchanged. Every tab identity is retained;
strip titles/CWD are explicitly elided on UTF-8 boundaries to 24/8 bytes.
The compile-time budget covers all five windows at sixteen tabs each, every
tab record, all built-in theme names, configuration path, and framing. Full
inventory navigation uses separate bounded pages (at most 1052 bytes with a
64-byte query and four 240-byte display labels).

## Focused validation

The optional `build.navigation.zig` test driver uses the actual shipping build
graph and gives its engine module a dedicated test root. Imported-module tests
do not otherwise run in the extension test executable. This gate compiles the
shipping TS/markup and runs navigation/codec contracts with same-checkout FFI.

From the repository root:

```sh
PATH="$HOME/.cargo/bin:$PATH" cargo build --locked -p phux-client-ffi
cd clients/cockpit
./scripts/zig-build.sh --build-file "$PWD/build.navigation.zig" navigation-check \
  -Dphux-enabled=true \
  -Dphux-client-ffi-lib-dir="$PWD/../../target/debug" \
  -Dphux-client-ffi-include-dir="$PWD/../../crates/phux-client-ffi/include" \
  --summary all
npm install --ignore-scripts
node --import ./src/tests/navigation-loader.mjs --test ./src/tests/navigation.test.mjs
```

The build-file path is absolute because Zig's alternate relative build-file
path leaves a dependency's test-case directory relative, tripping its absolute
path assertion. The JS loader uses Node's built-in TypeScript stripper to load
the pinned SDK's deliberately published TypeScript sources.

The focused gate is not integration acceptance. The parent engine/bridge must
install the hooks above, adapt the existing synchronous-switcher tests and
`ts-overlay-switcher` guard, and run the full Phux-inclusive suite and serial
live-host acceptance. No screenshot here claims macOS rendering evidence.

## Regression evidence

The snapshot regression was observed failing at `encodeTabs` with
`BufferTooSmall` after restoring the original 128-byte title/CWD allowances.
Its fixture uses 32 actual local terminals across two windows, each with a
maximum title and a verified 128-byte OSC-7 directory. Restoring the compact
allowances passes. The adopted-window JS check was observed failing with
`actual: 0, expected: 255` after restoring the former forced-main command.

These direct red/green runs do not constitute a recorded `.guard`: the guard
recorder requires a green full-suite baseline before its scoped graph. That
baseline depends on the parent hooks and replacement of the old switcher guard.
Record the new guard with `guard-red-run.sh` once integration has that baseline.

## Complexity and review evidence

Measured with ESLint's `complexity` rule and `@typescript-eslint/parser` against
the original main revision and this implementation:

| Function | Before | After |
|---|---:|---:|
| `protocol.snapshot` | 59 | 6 |
| `core.update` | 89 | 87 |
| `core.initialModel` | 1 | 1 |
| `core.withSwitcher` | 9 | removed |
| `core.switcherRows` | 10 | removed |

New navigation helpers are at most 9; extracted snapshot section readers are
at most 10. The legacy `update` dispatcher remains a hotspot. The Native
frontend rejects command-producing helpers with NS1017, requiring `Cmd`
construction inline in `update`'s return path; its exhaustive effect dispatch
was retained. This is measured remaining complexity, not a threshold pass.

Review fixes include clearing rows on invalidation before a new snapshot,
rejecting late replies by revision/query/page, rejecting short pages that hide
inventory, withdrawing connectivity claims when the engine fails, maintaining
backward keyboard order across pages, and preserving integer proofs required
by the actual AOT compiler. The independent integrated review remains with the
parent's engine/bridge acceptance.
