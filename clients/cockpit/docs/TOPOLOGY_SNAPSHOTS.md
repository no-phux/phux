---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-07
---

# Topology Snapshots

**TL;DR.** Version 5 preserves mixed local and remote pane trees. Local leaves
recreate ephemeral shells; remote leaves retain bounded references and original
endpoint, server-incarnation, and session evidence. Restored remote placements
stay pending until matching provider evidence establishes readiness. Satellite
references remain unresolved because coordinator identity does not prove a
satellite incarnation.

`Model.topologySnapshot()` is the durable boundary between terminal identity
and a live local process. The current version is `5`, and
`process_restoration_supported` is explicitly `false`.

## Persisted State

- The ordered **window** list, window 0 first (the scene's own window)
- The ordered tab list, flat across every window: each window owns the
  contiguous run of `tab_count` tabs that follows the windows before it
- Each tab's **pane tree**: leaves naming local IDs or remote references, branches
  carrying an orientation and a divider fraction
- Each window's selected tab — numbered **within that window's run** — and each
  tree's focused leaf
- Each terminal's working directory, when one was reported
- A bounded remote reference table: provider ID, tagged remote terminal ID,
  full host (at most 255 bytes), endpoint, opaque `HELLO_OK.server_id`, session ID

## Windows

Tabs are stored once, in one flat array, in window order. A window records how
many of them are its and which one it had selected; the offset into the array is
derived (`TopologySnapshot.windowTabOffset`). There is deliberately no per-tab
window tag: a tag and a count are two encodings of one fact, and only one of
them can be wrong. `validate()` proves the windows' counts account for the tab
array exactly, so a snapshot can neither strand a tab no window owns nor let a
window read past its own run.

The tab ceiling is per window (`max_tabs`, 16); the whole-session ceiling is
`max_snapshot_tabs` (32); combined local and remote leaves also cap at 32.
Windows cap at `max_snapshot_windows` (5: the scene's
window plus the four the toolkit budgets).

## Deliberately Ephemeral State

- PID and PTY transport handles
- PTY effect keys and spawn generations
- Emulator cells, scrollback, selection, and pending input
- Process phase, exit status, and clipboard operations

## Pending Remote Attachments

A snapshot records placement intent and identity evidence, not remote survival.
`SnapshotNode.remote_ref` indexes `TopologySnapshot.references`; local nodes
retain the existing `terminal` registry offset and working-directory semantics.
Every reference is used exactly once, and duplicate qualified IDs are rejected.
The supported provider is Phux; unknown provider and remote-kind tags are refused.

Restoring retains remote leaves in their exact tree positions and marks them
pending. `normalizeTopology()` preserves pending leaves. Pending state does not
grant `containsTerminal`, `selectedTerminalRef`, `terminalOwner`,
`ownerIsCurrent`, or `remotePresentation` readiness, even if another connection
publishes the same numeric ID.

Each saved reference keeps its original context across connection changes and
resaves. Endpoint matching is byte-exact; aliases are not inferred. Empty
endpoint or server identity means unknown and cannot match. The server identity
is opaque bytes, not a display name or a hash. Satellite IDs are retained but
never automatically matched: `HELLO_OK` proves only the connected coordinator's
incarnation. Per-route incarnation evidence would need an explicit future seam.

`restoreModel()` validates references and uniqueness before allocating fresh
libghostty-vt sessions for local leaves only. Starting the restored app spawns
one new shell per local terminal, in its recorded working directory when one
survived validation. Stable terminal IDs and pane geometry survive; the old
processes do not.

### Model integration API

All signatures below are methods of `cockpit/model.zig`'s `Model`; `TerminalRef`
is the provider contract type. They are additive internal APIs.

```zig
setAttachmentContext(model: *Model, endpoint: []const u8,
    server_id: []const u8, session_id: u32) !void
rejectAttachmentContext(model: *Model) void
pendingRestoredRefs(model: *const Model, out: []TerminalRef) usize
restoredAttachmentMatches(model: *const Model, ref: TerminalRef) bool
restoredAttachmentContext(model: *const Model, ref: TerminalRef) ?attachments.Context
resolveRestoredAttachment(model: *Model, ref: TerminalRef) bool
attachmentPending(model: *const Model, ref: TerminalRef) bool
pruneAttachmentState(model: *Model) void
canAddPane(model: *const Model) bool
```

1. Publish endpoint + `HELLO_OK.server_id` + attached session ID through
   `setAttachmentContext`, before admitting new remote placements. The method
   freezes existing placement evidence before changing the connection context.
   Endpoint and incarnation ceilings are 256 and 255 bytes; overlong inputs
   return an error without truncation.
2. Enumerate pending refs into a 32-entry buffer. A smaller buffer returns only
   the prefix that fits. Request subscriptions only for refs whose
   `restoredAttachmentMatches` returns true. `restoredAttachmentContext` exposes
   the saved endpoint/incarnation/session evidence when selecting a session;
   `attachments.Context` lives in `cockpit/attachment_state.zig`.
3. Once the provider publishes that ref with phase `.live`, call
   `resolveRestoredAttachment`. It repeats the context check and refuses missing
   or non-live provider presentation. Matching context alone never clears pending.
4. On disconnect or rejected context, call `rejectAttachmentContext`; pending
   leaves and their original evidence remain saved. Reconnect may retry them.
5. Before creating a shell or remote resource for a new leaf, check
   `canAddPane()`. The combined limit includes pending references. `admitTab`
   enforces this itself; split/spawn callers must preflight before allocation.
   After direct tree removals, call `pruneAttachmentState()` before admitting
   another identity. Model tab/window close and normalization already do this;
   moves retain evidence whenever their destination still holds the reference.

The composition/provider layer owns HELLO/session evidence and subscription
requests. It must use these fences before input, focus, or subscription paths
that directly access the provider rather than going through model readiness.

## Working Directories

Directories are a side table keyed by **registry offset**, not by tree node.
`validate()` already proves every leaf names a distinct terminal, so one entry
per terminal (32) replaces one path per node (31 per tab, on a by-value struct)
— a few kilobytes instead of hundreds.

A directory is recorded only if it can actually be restored. `SnapshotCwd.set`
refuses a relative path, an embedded NUL, an embedded newline, or an over-long
value, so "not recorded" degrades to `$HOME` and never to `/` or to a truncated
path that names a different directory. Restoring one goes through
`local.paneArgvIn`, which single-quotes the path and escapes `'` as `'\''` —
the only quoting with no escape sequences of its own — and falls back to `$HOME`
with `;` rather than `&&`, so a directory that has since moved yields a normal
shell instead of a pane that exits the moment it opens.

## Canonical Invariant

Every accepted snapshot restores exactly and captures back byte-for-byte at the
struct level; restore never clamps or normalizes current-version topology.
Validation therefore rejects non-canonical state:

- terminal IDs are unique across ALL tabs; local IDs are valid registry offsets
  and remote nodes name valid, distinct entries in the bounded reference table
- every tree is a tree: no cycles, no node reachable as a child twice, no
  orphans, exactly one root
- a branch names two distinct existing children; a leaf names a terminal
- focus names a **leaf**, never a branch or a free slot
- each window's selection references a tab that window owns
- the windows' tab counts sum to exactly the tab list's length, so no tab is
  orphaned and no window reads past its own run
- windows past `window_count` are blank
- divider fractions are finite and already within `0.05...0.95`
- recorded working directories are absolute and free of NUL and newline
- an empty registry has no tabs and selects nothing

`Model.topologySnapshot()` returns an error if the projected topology is
not canonical or exceeds the bounds — it never
emits an invalid snapshot and never rewrites the live model to make one valid.
Migration from an older version may explicitly normalize its less expressive
representation before emitting a validated current snapshot.

## Migration

`migrateTopologySnapshot()` accepts the tagged `PersistedTopologySnapshot`
union.

- **Version 0** stored only terminal count, selected index, split state, and
  divider fraction. Migration assigns the original terminal-ID sequence and
  reconstructs a layout from it.
- **Version 1** stored a flat tab order plus at most two attachments and one
  split fraction — the two-pane model that predated pane trees. A v1 split
  becomes ONE tab holding a horizontal branch over its two attachments, which
  is what that state always meant.
- **Version 2** is byte-identical to version 3 except for working directories,
  so its migration is exactly "no directory was recorded".
- **Version 3** is one window's worth of tabs plus a single file-level
  selection — the schema from before windows existed. It migrates to a v4
  snapshot with `window_count == 1` whose only window owns every tab it
  carried, so **a pre-multi-window snapshot restores as exactly one window**.
  A v3 file with no tabs stays a zero-window snapshot, which is what "there is
  nothing to reopen" has always meant.
- **Version 4** retains its windows, local leaves, working directories and
  selection unchanged, adding an empty reference table. New callers use the
  `.v5` persisted union arm; `.v4` remains accepted by legacy internal callers.

Unknown versions are not guessed. Invalid counts, duplicate or exhausted IDs,
dangling references, malformed trees, focus on a non-leaf, and out-of-range
divider values are all rejected in favour of a normal fresh launch.

## On-Disk Form

The snapshot is written to the platform **state** directory (not the config
directory — layout is state) as a flat, line-oriented text file terminated by an
explicit `end`:

```
phux-cockpit-state 5
placement top
window tab 1
tab 0 0
node 0 leaf - 0
tab 2 1
node 0 leaf 2 1
node 1 leaf 2 2
node 2 branch - horizontal 0.5 0 1
window web
tab 0 0
node 0 leaf - 3
end
```

Remote records precede the windows and use this grammar:

```text
ref INDEX PROVIDER_U64 KIND_U32 ID_U32 HOST_HEX ENDPOINT_HEX SERVER_ID_HEX SESSION_U32
node INDEX remote PARENT REF_INDEX
```

Hex encoding round-trips spaces, NULs and newlines in opaque identity bytes;
`-` denotes an empty byte string. Reference indexes are dense and ordered.
The fixed file ceiling is 96 KiB, derived from 32 references with maximal host,
endpoint and incarnation lengths, local cwd lines, tab headers and live nodes.
Both serialization and parsing enforce it. Legacy v2-v4 files reject `ref` and
`remote` lines.
Coordinator-local remote IDs require an empty host; satellite hosts must be
nonempty UTF-8, matching the provider's canonical identity validation.

A `window` line OPENS a window's run, and every `tab` line after it belongs to
that window until the next `window` line — so a window's tab count is implied
by the file's own structure rather than written down twice. The window's own
`web`/`tab N` argument is its selection, numbered within its run. A `tab` line
before any `window` line is a tab with no owner and rejects the file.

Version 3 and earlier have no `window` line and carry one file-level
`selection` line instead; a `selection` line in a v4 file is a parse failure,
because it is a claim the schema stopped making.

The format is deliberately non-nesting. This file is read at startup, from a
user-editable path, and may have been half-written by a crash. A recursive
parser over a nested document has stack depth proportional to its input, which
turns "a file of random bytes must never crash" into a claim about how deeply
the bytes happened to nest. A flat grammar is one forward pass with fixed work
per line and no recursion, so it is structurally incapable of overflowing a
stack or looping. Structural claims stay where they already were, in
`validate()`; the parser only fills fields.

Truncation is **detected, not inferred**: a write cut short fails the terminator
check instead of parsing as a smaller but entirely plausible workspace.

Writes are debounced and edge-triggered off a hash of the workspace *shape*,
full qualified terminal identity and saved attachment context.
That hash deliberately excludes working directories — folding them in would
make every `cd` a disk write. A shutdown flush is synchronous, because nothing
drains an effect queue after shutdown.
