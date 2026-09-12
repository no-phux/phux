---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-11
---

# Cockpit everyday UX: implementation design

**TL;DR.** Keep one terminal engine and the existing Phux authority boundaries.
Replace incomplete discovery and navigation surfaces with explicit machine,
session, window and command models. Start with a real saved-machine inventory,
then unify actions and navigation. Replace fixed coordinator slots deliberately.
Every slice must survive real input, failure and return-to-work journeys before
its user-facing claim is accepted.

## Context

This proposal implements [PRODUCT.md](PRODUCT.md), tracked by `phux-2jza` and
its children. Source baseline: `d5e2977c1e327c84fe20b1f8192589be693008c1`.
The source findings below are not live-app reproduction evidence.

| Current source | Consequence for the design |
|---|---|
| [`cockpit-window.native:77-155`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/windows/components/cockpit-window.native#L77-L155) | Switch workspace is a navigation catalog; Connect to Host is a typed registered-target form without saved-machine selection. |
| [`ts_navigation.zig:12-49`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/cockpit/native/ts_navigation.zig#L12-L49) | Four results are a fixed page budget derived from minimum window height. Host targets are filters, not connection commands. |
| [`remote/target.rs:1-21`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/crates/phux-client-ffi/src/remote/target.rs#L1-L21) | FFI already shares the Phux registry loader; pairing stays in the CLI. Expose inventory from this boundary rather than adding a second config parser. |
| [`model.zig:725-800`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/cockpit/model.zig#L725-L800) | Peer ownership, failures, workspace projections and restore state are parallel fixed-size arrays. |
| [`phux_support.zig:332-382`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/cockpit/phux_support.zig#L332-L382) | Generation channel IDs reserve sixteen slots; increasing the peer constant can alias events. |
| [`remote_memory.zig:32-44`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/cockpit/remote_memory.zig#L32-L44) | Persistence separately caps saved reconnect records at three hosts. |
| [`cockpit-window.native:252-345`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/windows/components/cockpit-window.native#L252-L345) | Settings offers appearance controls and Finder reveal, not editor launch or a searchable settings catalog. |
| [`core.ts:2150-2152`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/src/core.ts#L2150-L2152) | Reveal is a guarded host intent. An editor launch needs a correlated local creation result, not a cosmetic replacement label. |
| [`app.zon:13-99`](https://github.com/no-phux/phux/blob/d5e2977c1e327c84fe20b1f8192589be693008c1/clients/cockpit/app.zon#L13-L99) | Shipping menus include Connect to Host, Go to Directory and Rename Session, but no machine inventory or window list. Cmd+Shift+P currently opens Go to Terminal. |

The existing native/shared-workspace engine remains the authority for topology,
terminal identity, input routing and geometry. The TypeScript core and compiled
markup remain the presentation shell. Human-facing vocabulary does not introduce
a new wire collection tier or a second durable coordinator.

## Proposed changes

### Saved machines, connection, and enrollment

Add a read-only saved-machine inventory beside the existing remote resolver in
`phux-client-ffi`. Use `phux-config` for paths/schema and return versioned,
caller-bounded records containing stable registry identity, display name and
endpoint metadata. Do not return token contents or read credentials just to list
machines. Include disconnected entries and explicit resolution errors.

The CLI's `host ls` also represents satellite roles. The inventory adapter must
preserve role/route information: a satellite reached through a hub is not
automatically a directly connectable remote coordinator. Product labels explain
the supported route instead of dropping the machine or inventing a direct dial.

Expose inventory through a dedicated request/response seam with cancellation and
request identity, separate from terminal snapshot and navigation buffers. The
native engine joins saved records to current connection state by registry/endpoint
identity. A saved alias is not evidence that an existing provider is authenticated
as that endpoint. Registry changes invalidate captured connect targets; re-read
and resolve at activation, reporting changes rather than dialing a new destination
under an old row.

Machines is an explicit shared chrome template, usable from every window. It
separates Browse Sessions, Connect, Retry, Disconnect and Forget. Preserve the
existing CLI registry on Disconnect; introduce Forget through the same
comment-preserving registry mutation path the CLI uses, with exact entry identity.
Remembered reconnect preference and saved registration are different data.

For interactive enrollment, reuse the CLI's authentication/setup behavior through
a dedicated local Phux terminal. Capture executable identity, structured argv,
target and result correlation; never inject a shell command into existing work.
After setup, refresh the registry and resolve the requested machine again. Do not
equate child process exit with a working remote connection. Missing local Phux or
incompatible versions use the installation/composition lane's recovery contract.

### Navigation and command ownership

Introduce focused presentation models for Sessions, Machines, Windows and
Commands. Each owns its query, selection and result state. Share identity-based
selection, row presentation, focus return and loading/error conventions; do not
encode every flow as another boolean in the existing global modal dispatch.
Retain the sole native terminal engine and its correlated operation receipts.

Build command presentation from the shipping `app.zon` command/shortcut
declarations and capability predicates. Retained native scene fixtures have
different menus and must not be mistaken for the shipping source. Commands executes
the same captured action as menus and pointer controls. New Session, configuration
editing and machine setup must have functioning operation paths before they are
advertised as executable actions.

Window enumeration comes from the real open platform windows and their topology
projections. Window/tab selection carries stable identities, never a displayed
row number. Raising/minimized-window recovery uses the existing platform focus
effect; investigate and fix its host seam if the real AppKit path cannot fulfill
the product behavior.

The current `ts_engine.zig:1208-1226,1790-1819` leaves one shown session to show
another on the same coordinator. To satisfy product behavior 18, separate
coordinator identity/catalog from per-session attachment and workspace projection.
A coordinator owns session attachments keyed by session id plus creation time;
windows select projections, not a single mutable provider-wide selected session.
Share an attachment when windows show the same session, and release it only when
no visible view needs it. First prove whether the current FFI/kernel can hold the
required subscriptions on one connection; if not, use an independently bounded
connection per visible session under the same coordinator identity. Terminal
actions carry their resource and invoking attachment/view context. A second
window must not replace the first window's subscription or mutate its focus.

Rendering capacity and transport pagination are separate. Request bounded pages
and fill a scrollable, viewport-sized list without unbounded snapshot growth.
The current 4096-byte seam budget does not justify a permanently four-row UI.
Reconcile asynchronous pages by query/request identity and stable target; retain
position through refresh and invalidate actions whose target disappears.

### Coordinator lifecycle and scale

Before changing the four-coordinator limit, measure per-provider idle/attached
memory, channel/worker limits, reconnect behavior and catalog costs on the pinned
SDK. Replace parallel peer arrays with owned coordinator entries containing
provider, connection incarnation, workspace, retry and restore state. References
must remain valid across collection growth; event routing never borrows an array
position as identity.

Use allocated channel handles mapped to coordinator identity plus incarnation,
with checked retirement, instead of the sixteen-slot arithmetic. Stale channel,
timer, selection, cleanup and mutation results cannot resolve to a replacement
entry. Preserve current conditional-kill and focus-restoration invariants.

Keep registration/catalog capacity independent from connected providers and
attached terminals. Only visible sessions attach and contribute geometry. Idle
connection scheduling must not disconnect visible work to make a background list
fresh. Choose budgets from measurement and report exhaustion explicitly.
Migrate v1/v2/v3 remote-memory records without silently truncating the catalog;
unknown future data and failed writes preserve the source file. Record any change
to accepted lifecycle/restore policy in an ADR before implementation.

Product behavior 19 chooses conventional terminal-ending Close Pane/Close Tab
and non-destructive Phux Close Window/Quit. This is a deliberate change from
Cockpit's current layout-only durable tab-close paths, not permission to relabel
them. Write a lifecycle ADR before implementation, reconciling
[ADR-0105](../../ADR/0105-sessions-can-outlive-their-last-window.md),
[ADR-0107](../../ADR/0107-satellite-sessions-are-listed-never-adopted.md), and
the [emptied-window decision](../../clients/cockpit/docs/DECISIONS.md).
Retain ADR-0107's satellite leaf termination and ADR-0105's opt-in keep-empty
semantics; Cockpit New Session opts in. Specify the keep-empty window exception
and replace stale close-window wording in retained `ts_protocol.zig` contracts.
Use existing all-or-nothing resource teardown for Close Tab/End Session where
applicable; acknowledge completion before removing live views. Failed or partial
remote operations must retain accurate per-terminal state. A second CLI observer
must prove which resources survive every action in the product lifecycle matrix.

### Settings and configuration editing

`src/config/config.zig:325-400` already represents font family/size, system-theme
following, color overrides, cursor style/blink, contrast, scrollback, shell,
working-directory inheritance and tab placement, beyond the few exposed controls.
Trace each setting to its real consumer and application timing before enabling
its control; a parsed field does not prove a setting affects Phux-backed panes.
Preferred editor and editable keyboard bindings need explicit supported behavior
rather than placeholder controls. Add one settings description source with labels, groups,
defaults, applicability and application timing. Keep server/TUI-owned settings
separate. Expand the existing comment-preserving writer and preview transaction;
do not introduce a competing config file or browser-only state store.

Preferred editor resolution belongs to a local-launch service shared with setup
terminal creation. It must handle Finder's environment, explicit editor preference,
VISUAL/EDITOR command arguments, unavailable programs and paths with spaces.
Launch structured argv on the local Phux provider even when a remote pane is
focused. Keep file creation separate from a failed launch and preserve existing
content. Reload reports parse errors against the active file and retains last-good
values. Detect conflicts between an external edit and a pending Settings preview.

The minimum control catalog follows product behavior 26. Defaults below refer
to the code defaults at the pinned baseline; inherited values show their actual
source instead of copying a static label. Any intentional default change must
be recorded with the implementation.

| Setting/control | Default/source | Owner and applicability | Editing and application |
|---|---|---|---|
| Font family / size | Resolved platform font / 13pt | Cockpit, all terminal views | Editable, live preview; derive actual font when family is empty. |
| Theme / follow system | Current resolved theme; follow-system false | Cockpit presentation | Editable live; explicit foreground/background precedence remains visible. |
| Contrast | 3 | Cockpit rendering | Editable live; preserve source colors and engine state. |
| Cursor style / blink | Block / true | Local terminal defaults; remote application may own cursor | Editable with applicability; remote behavior cannot be claimed until its owner supports it. |
| Scrollback | 50 MiB for the local config | Local engine retention; remote history/cache is separately owned | Editable for supported owner; explain byte units and newly-created-terminal applicability until live resizing is proven. |
| Shell | User login shell | Local scratch spawn default; Phux spawn defaults belong to serving user | Editable local preference; remote effective value is read-only with its config/edit route. |
| Working-directory inheritance | true | Creation action on focused resource's host | Editable, applies to new terminals/splits. |
| Tab placement | top | Cockpit presentation | Editable live. |
| Single-terminal chrome / padding | true / 8pt in config | Cockpit presentation; trace shipping consumer first | Expose only with real behavior; defaults cannot hide the product's required discovery controls. |
| Preferred editor | Explicit choice, VISUAL, EDITOR | Local setup/config editing | Editable, next editor launch; render resolved executable and arguments. |
| Keybindings | Shipping app.zon bindings | Cockpit commands, not TUI/server bindings | Search, remap and reset; validate conflicts and update actual runtime registration before claiming success. |
| Phux destination / session | Resolved value plus environment/config provenance | Connection lifecycle | Read-only effective display with Machines/session selection routes; changing a form field cannot silently retarget work. |

### CLI, bundle and local-server composition

Cross-lane tracking is `phux-8ghp` (composition) and `phux-qnps` (release
resolution). Current `src/providers/phux/startup.zig:19-47` discovers only a
bundled sibling and discards helper stderr. The integration contract is:

| Situation | Selected behavior and recovery |
|---|---|
| Compatible local server already runs | Attach to that server at the resolved socket. Do not upgrade or replace it merely because another CLI is discovered. |
| No local server; compatible independently installed CLI exists | Prefer the explicit configured candidate, otherwise an installed candidate proven compatible by a bounded read-only protocol/capability probe; ensure the server at the resolved socket. |
| Cockpit-only installation or installed CLI is unsupported/unknown | Use the same-checkout bundled CLI fallback. Do not overwrite the independent CLI or treat semver comparison as a compatibility test. |
| Explicit socket override | Address that exact socket throughout probe, ensure, attach, setup and editor launch. Failure names that destination; never retry against the default socket. |
| Finder environment lacks shell PATH/EDITOR | Resolve candidates using the runtime-discovery contract and explicit preferences; use bundle fallback and editor selection. Do not source arbitrary shell startup files just to inspect versions. |
| Running server and Cockpit cannot negotiate | Keep work and its server intact. Show which component is incompatible and an Update Cockpit or Update Phux recovery appropriate to supported releases, then Retry. A Phux update uses its existing graceful-upgrade path; a bundle update alone does not restart the server. |
| Helper launch/ensure fails | Keep bounded stderr/exit evidence, identify the executable and socket, offer Retry or Repair Installation; do not report a generic disconnected terminal or fall back to scratch. |
| Compatible CLI installed after Cockpit | Apply preference at the next server-start decision, not by moving active work. An existing compatible connection remains authoritative. |

Setup and editor terminals use this same selected local coordinator even when a
remote terminal is focused. Runtime probe output must be versioned and usable
without connecting or changing a server. Candidate resolution is deterministic,
bounded and captured once per launch operation. Repair results return to the
initiating Machines/Settings flow with state refreshed from the real server;
an updater success line alone is not proof of a working terminal.

## Testing and validation

Before changing a live flow, capture baseline behavior with app path, source/build
identity, publisher PID, config path and server identity. Source inspection above
is sufficient to plan work; it does not establish current on-glass behavior.
Use the isolated app workflow in [SETUP](../../docs/SETUP.md#cockpit); live app
automation is serial, and publisher identity is checked before every journey.

| Product behavior | Required evidence |
|---|---|
| 1-6, 15-18 | Run local/remote sessions with similar names, multiple windows and overflow; locate and raise existing work through keyboard and pointer; preserve split contents and exact target across refresh. |
| 7-14 | Empty, populated and malformed registry; disconnected/SSH-only/satellite entries; external registry edits; add/authenticate/connect/fail/cancel/retry/disconnect/forget. Use a real enrolled remote for final transport acceptance (`phux-c2td.17`). |
| 13 | More than four registered and connected hosts; dynamic allocation failures; delayed old channel/cleanup events; reconnect storms; measured idle memory, time to interactive list and input responsiveness under catalog load. |
| 19-21 | Process exit, view closure, link loss, quit/relaunch, graceful upgrade and cold restart; verify retained work, truthful state and newer-user-choice precedence. |
| 22-25 | Find and execute every offered action, verify actual shortcut bindings, IME editing, no-result/disabled states, Escape/focus return, stale captured action and screen-reader labels. |
| 26-30 | Config catalog/applicability, preview/cancel/save/reset, external edit conflicts, malformed/unwritable/missing config, chosen editor argv and correct local file from remote focus. Observe a real editor terminal and reload. |
| 31-33 | Installer lane's actual installed/available versions, repeat installs, component compatibility and update recovery, then launch Cockpit against the resulting local Phux. |
| 34 | Real shell and fullscreen TUI input/IME, pointer selection, copy/paste, search, links, wheel scrolling and split-drag journeys while output/history/catalog updates run. Record wrong-target or duplicated input and compare input-to-paint behavior to the same-machine baseline. |

Scoped checks start with `bash scripts/doctor.sh cockpit` and `just cockpit-test`
using same-checkout FFI, followed by `just cockpit-build`. Shared Rust/FFI/config
changes require affected crate checks and the expanded root gates documented in
[SETUP](../../docs/SETUP.md). Run docs checks for these specifications.

Named bug regressions must fail against the actual prior behavior before passing
with the fix. Measure touched branching functions before/after using project
tooling; avoid a broad rewrite merely to change a score. Independent review must
cover ownership, stale actions, lifecycle, usability and missing evidence.

Compiled layout/accessibility checks cover all declared sizes, secondary windows,
top/side tabs, long labels and failure states. CPU-reference captures demonstrate
layout, not CoreText fidelity; follow
[render fidelity](../../clients/cockpit/docs/RENDER_FIDELITY.md) for on-glass claims.
Preserve input/scrolling/selection and terminal geometry throughout. Results must
name which product behaviors were observed, still fail, or remain untested.

## Delivery boundaries

The parent owns this design and the integrated navigation/settings experience in
the isolated `feat/cockpit-everyday-ux` worktree. The independent installation and
update session owns its own isolated branch; integrate its verified result at the
local-runtime/setup boundary rather than duplicating discovery code here.

Begin with a complete saved-machine browse/select/connect journey, then command
discovery and window/session navigation, followed by settings/editor integration
and measured coordinator scaling. These are implementation boundaries, not claims
that a partial slice completes the product contract. Current task state and
acceptance evidence stay in Beads, not this document.
