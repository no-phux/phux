---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-10
---

# Shipping navigation seam

**TL;DR.** The TypeScript core presents a four-row native catalog with
revision-fenced page reads, optional session and host scopes, and captured
provider-qualified selection targets. Selection and its admission receipt use
the bounded interaction command FIFO; known-host rows carry a filter token that
only narrows the view. Snapshot byte 23 reports provider connectivity
independently of engine readiness.

## Engine hook contract

`src/cockpit/native/ts_navigation.zig` exports:

```zig
pub fn encode(model: *const Model, revision: u64, request: []const u8,
              out: []u8) Error![]const u8;
pub fn resolve(model: *const Model, current_revision: u64,
               expected_revision: u64, index: u16) ?PaletteDestination;
pub fn connection(model: *const Model) Connection;
```

Both catalog functions are read-only; `resolve` is a compatibility positional
route. The shipping core activates captured opaque targets through
[`INTERACTION_SEAM.md`](INTERACTION_SEAM.md#provider-qualified-catalog-selection).
The engine must advance its page revision
whenever catalog membership, order, placement, or session identity changes.
That includes provider catalog replacement and reconnect, even when the visible
tab topology stays the same. Selection must resolve and activate on the owning
thread without an intervening catalog mutation.

`ts_protocol.decodeNavigationIntent(payload)` retains the existing 12-byte
intent envelope with two tags:

| Tag | Meaning | Bytes 10-11 |
|---|---|---|
| 12 | Reconnect Phux through the existing connection lifecycle | unused; TS sends 0,255 |
| 13 | Compatibility positional catalog activation; not emitted by shipping chrome | little-endian **u16 index** |

Bytes 2-9 remain the little-endian expected revision. Decode these tags before
legacy window adoption: the high byte of a catalog index is **not** a window.
Existing `IntentKind` and `NativeCommand` discriminants are unchanged.

The compatibility tag 13 calls `resolve(model, engine.revision, intent.expected_revision,
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
| 1 | kind 3, or 4 for a scoped request |
| 2 | expected revision, u64 |
| 10 | filtered offset, u16 |
| 12 | UTF-8 query length, u8, at most 64 |
| 13 | query bytes |

The response echoes the complete request, then appends `total:u16`, `count:u8`,
and `count` records with a display index, opaque target, and label. The exact
bounded record and target layout lives in the
[interaction seam](INTERACTION_SEAM.md#provider-qualified-catalog-selection).
The index refers to the unfiltered catalog for display bookkeeping only;
activation echoes captured target bytes rather than reconstructing identity.

Scoped requests use kind 4 and append `scope:u8, host_length:u8, raw_host` after
the query. Scopes are all work (0), sessions (1), known terminal hosts (2), and
exact host (3). Only exact-host requests carry a host, up to 255 bytes; empty
host denotes the coordinator. Local ephemeral PTYs do not appear in host lists.
`navigationRequest` retains the original three-argument TS API;
`navigationScopedRequest` takes explicit scope and host arguments.

Responses append marker `0x4e` after the records, followed by one metadata
record per row: `kind:u8, selectable:u8, detail_length:u8`, then at most 160
detail bytes. Kinds distinguish open terminals (0), available terminals (1),
sessions (2), and hosts (3).

Terminal and session rows carry a captured catalog target (tag 2); only those
rows enqueue a catalog command. A known-host row instead carries the filter
token `3, host_length:u8, raw_host` in its target slot, and kind 3 must pair
with exactly that token. Activating it switches to the exact-host scope with
the token's raw host. It never enters the command FIFO or sends a catalog
activation, even when held across a replacement page.

Unknown ownership makes an available row disabled. The core refuses keyboard
or painted submission of a current disabled row, and the engine refuses any
target it cannot admit. A held painted action is not revision-bound: it echoes
its captured identity, so a replacement page that reuses its index cannot
retarget it.

The existing workspace projection supplies enumeration and matching: every
placed pane in every open window, every unplaced remote ref, then every remote
session. Display labels are the terminal title or session name. Window, tab,
host and directory are carried in the metadata detail, and availability in the
row kind and selectable flag.
Search uses the projection's full metadata rather than the compact strip label.
An inventory larger than u16 is an explicit error, never a silently shortened
result. Current bounded model/provider inventories fit comfortably.

Four 40pt two-line rows fit the 420pt minimum: 32pt outer padding, 32pt panel
padding, 32pt heading, 32pt scope controls, 40pt input, 160pt results, up to 20pt
notice, 32pt paging controls, and five 8pt gaps. The compiled layout gate checks
populated results at the minimum size.
Previous/Next work by pointer; arrow navigation crosses page boundaries. The
query, offset, revision, scope and raw host are echoed so late responses cannot
replace newer results. Invalidation withdraws current rows and disables keyboard
submission; a held painted action retains its original authority for native
validation. Refresh is always reachable, including an error or empty result.

## Snapshot and connection state

Snapshot byte 23, formerly reserved, carries `local=0`, `connecting=1`,
`connected=2`, `offline=3`, or `workspace_unavailable=4`. Engine `status=READY`
means its snapshot was accepted; the visible `connectionStatus` is separately
derived from the provider.
An unavailable engine withdraws the connectivity claim. An offline provider
exposes Reconnect in main and secondary window chrome.

The toolkit's 4096-byte limit is unchanged. Every tab identity is retained;
strip titles/CWD are explicitly elided on UTF-8 boundaries to 20/8 bytes.
The compile-time budget covers all five windows at sixteen tabs each, every
tab record, all built-in theme names, configuration path, and framing. Full
inventory navigation uses separate bounded pages (at most 3162 bytes:
`19 + 64 + 255 + 4 * (8 + 298 + 240 + 160)` for the scoped request echo, page
framing and metadata marker, then four opaque targets, display labels and
metadata details). Snapshot extension 3 carries current attached session,
coordinator endpoint and connection detail; it describes real provider state
rather than startup session configuration.

## Focused validation

The optional `build.navigation.zig` test driver uses the actual shipping build
graph and gives its engine module a dedicated test root. Imported-module tests
do not otherwise run in the extension test executable. This gate compiles the
shipping TS/markup and runs navigation/codec contracts with same-checkout FFI.

From the repository root:

```sh
PATH="$HOME/.cargo/bin:$PATH" cargo build --locked --profile ffi-release -p phux-client-ffi
cd clients/cockpit
./scripts/zig-build.sh --build-file "$PWD/build.navigation.zig" navigation-check \
  -Dphux-enabled=true \
  -Dphux-client-ffi-lib-dir="$PWD/../../target/ffi-release" \
  -Dphux-client-ffi-include-dir="$PWD/../../crates/phux-client-ffi/include" \
  --summary all
npm install --ignore-scripts
node --import ./src/tests/navigation-loader.mjs --test ./src/tests/navigation.test.mjs
```

The build-file path is absolute because Zig's alternate relative build-file
path leaves a dependency's test-case directory relative, tripping its absolute
path assertion. The JS loader uses Node's built-in TypeScript stripper to load
the pinned SDK's deliberately published TypeScript sources.

The focused gate is not integration acceptance. The shipping engine/bridge now
implements the hooks above, and the full suite exercises asynchronous catalog
completion, exact cross-window pane selection, remote/session activation, and
independent snapshot/catalog completion slots. Serial live-host acceptance is
separate. No screenshot here claims macOS rendering evidence.

The integration gate is `./scripts/zig-build.sh test -Dphux-enabled=true
--summary all` from `clients/cockpit`, after the same-checkout FFI build above.
The verified run reports 53/53 steps, 460 passed and 2 skipped tests, including
all 44 shipping extension tests. Its verdict names this worktree's source root
and `target/ffi-release` archive. The focused navigation gate and eight JS
behavior tests also pass.

## Historical regression evidence

The guard names and runner below describe the original RED/GREEN runs.
The permanent ledger is retired; current work follows the repository's
[mutation testing policy](../../../docs/TESTING_MUTATIONS.md).

The snapshot regression was observed failing at `encodeTabs` with
`BufferTooSmall` after restoring the original 128-byte title/CWD allowances.
Its fixture uses 32 actual local terminals across two windows, each with a
maximum title and a verified 128-byte OSC-7 directory. Restoring the compact
allowances passes. The adopted-window JS check was observed failing with
`actual: 0, expected: 255` after restoring the former forced-main command.

The integration guards additionally cover the full-suite snapshot budget and
the snapshot-commit fence. An invalidation enters internal `SYNCING` and retains
the last accepted revision until snapshot commit. Advancing the revision early
would pair old tab positions with new authority; leaving internal `READY` in
place also let test/runtime consumers mistake an announcement for a committed
snapshot. Navigation replies are withheld while that commit is pending.

Recorded with `guard-red-run.sh` against green default and Phux-enabled full
baselines: `ts-overlay-switcher`, `ts-snapshot-commit-fence`,
`ts-navigation-snapshot-budget`, and `ts-navigation-completion-isolation`.
Each named test was observed red with its recorded break and restored green.
Independent-review fixes add `ts-navigation-placement-recovery` and
`ts-navigation-refresh-highlight`, also recorded red and restored green.

## Historical complexity and review evidence

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
by the actual AOT compiler. A fresh-context, read-only Claude review inspected
the integration diff, provider lifecycle, and pinned SDK behavior. Its
navigation-owned findings are fixed: keyboard highlight survives metadata
refreshes and clamps when a page shrinks, successful remote placement clears
an old capacity refusal, same-session activation is documented and tested as
idempotent, and tags 12/13 are explicitly reserved from future legacy intents.

The review also identified a parent-owned deferred-reopen failure:
`onPhuxChannel` must announce the state after reopening on `.closed/.rejected`,
including immediate admission failure. A temporary regression was observed
failing on `expect(announced)` and handed to that method's owner. The SDK's
closing-channel retry window also belongs to that lifecycle coordination.
Neither finding is a reason to weaken the revision fence. Non-positional
command handling during synchronization remains on the existing all-intent
fencing contract; relaxing it needs runtime-ordering evidence. The bounded
four-row catalog's identity lookup remains unchanged; the review's performance
observation was not a measured latency regression. Live acceptance is separate.

Integration complexity against `58b77356`: ESLint measures `loadedNavigation`
9 → 10 and `core.update` 87 → 87. A lexical Zig decision inventory (comments
and strings excluded; `if`, loops, short-circuit operators, `catch`/`orelse`,
and switch alternatives counted) measures `applyIntent` 17 → 18, connection
mapping 6 → 7, bridge request 4 → 5, and all new engine navigation helpers
at most 7. The legacy intent dispatcher is a remaining hotspot; concurrent
creation/lifecycle integration owns its other branches. No unrelated dispatch
rewrite is included in this navigation lane.
