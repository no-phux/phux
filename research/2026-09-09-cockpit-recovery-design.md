---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Cockpit recovery: one runtime, explicit ownership, evidence from the real app

**TL;DR.** Keep the Native SDK for recovery. Consolidate Cockpit's terminal
behavior behind one native runtime, make input ownership independent of paint,
and require a command round trip through the actual app before declaring input
fixed. The initially audited 0.17.0 had a proven local-only input route, but
**installed 0.18.0 already contains substantial repairs**. Those are foundations
to verify and deepen, not missing features to implement again.

This is a proposed recovery design, not an accepted ADR or an implementation
completion claim. Beads `phux-h8x2` tracks the investigation; `phux-mz6e` tracks
the remaining program. Execution status belongs in Beads.

## 1. Establish which application we are talking about

The triggering report was connected Cockpit becoming unusable while adding
terminals: typing and Enter did nothing. No exact live reproduction of that
sequence was performed in this design investigation.

| Evidence | Initial audit | Installed artifact / corrected source |
|---|---|---|
| Cockpit | 0.17.0 | 0.18.0 |
| Phux source | `c6c37118ac084fd323ea2d4e9d42b29f60616045` | `6bb68787747fb196299a274a3676c3f346802fe0` from bundled FFI provenance |
| Native SDK pin | `34cc9d5571599d5ea4feafc9260f36575e67e77b` | `71bbce511187b9e071d3ca8cbf3b1b19138577da` |
| Installed executable SHA-256 | Not applicable | `afa89d6de48be22919b239a1f47d488b7969e2bae4bbb5383a7af0b2304a5f33` |

The parent read the installed bundle's version and provenance and computed its
executable hash. The test investigator also verified its signature and SDK notice.
Neither installed nor dev Cockpit was running at the initial process inspection.
The FFI provenance identifies its source revision; it does not by itself prove
that every executable byte is reproducible from that revision.

Fetching the repository exposed `aee36811`, **complete native durable Phux
interactions and recovery**, followed by release and soak fixes. The isolated
design branch was fast-forwarded to `6bb68787`. All current source references
below mean that revision. Historical findings are explicitly labeled.

This correction matters: calling the current app's keyboard path local-only
would be false. The original report still needs artifact-bound reproduction,
especially through real focus/responder input after creating a terminal.

## 2. What changed, and what the old failure teaches

At `c6c37118`, shipping `Engine.onKey/onText` called `focusedPane`, which resolved
only through `Model.provider: *LocalProvider`. The local registry rejects Phux
identities. Remote painting worked, while the input handlers returned silently.
The retained `update.zig` had remote dispatch, so its tests could remain green.

The parent ran the old full gate: **53/53 steps, 405 passed, 2 skipped**, Phux
compiled and tested, exit 0, correct worktree source root. That demonstrates the
old coverage gap; it is not evidence about current interactive behavior.

The current release has meaningfully improved the shipping path:

| Earlier finding | Current implementation to preserve |
|---|---|
| Remote key/text/paste absent | `cockpit/terminal_interaction.zig` plus `Engine.onRemoteKey/onText`; structured remote key/text and owner-qualified clipboard routing. |
| Resize drops remote panes | `terminal_interaction.resize` calls the provider viewport interface; shipping extension has a remote resize test. |
| Remote click/selection/wheel absent from ordinary route | `cockpit/native/shipping_pointer.zig` provides provider-qualified gestures. |
| Copy completion follows new focus | `copy_owner` and `ownerIsCurrent` fence completion. |
| Held-key releases follow new focus | `rememberKey/releaseKey` reuse owner tracking; validate repeat and cancellation separately. |
| Local-only New Terminal/Split | `durable_creation.zig` and FFI operations implement coordinator-backed creation, correlation and publication admission. |
| Close cannot dismiss remote; exit leaves empty topology | `workspace_lifecycle.zig` coordinates close; normal shell exit now calls it. |
| Missing remote navigation and health | `ts_navigation.zig`, paged TS navigation, explicit session/reconnect and separate provider connection status. |
| Snapshot cannot fit supported windows | `ts_snapshot.zig` derives a bounded budget with 24-byte title / 8-byte cwd strip elision; full metadata is available through paged navigation. |
| Invisible search | Native search painter and remote presentation commands provide visible query/results, rebootstrap and frozen-state handling. |

Paths in the table are under `clients/cockpit/src/` unless named as FFI.
These are source and test-path observations, not a fresh on-glass pass.
The current extension includes tests for emitted key/text/paste/focus frames,
control chords, composition modifiers, delayed clipboard ownership, durable
publication, frozen display, navigation and remote viewport changes. It is no
longer accurate to describe all Phux tests as merely a failed-connect fixture.

The architectural lesson survives the correction: **provider-neutral identity
and painting do not guarantee complete terminal behavior**. The actual
application must exercise the same implementation as its behavioral tests.

### Residual findings at the corrected revision

These are source findings, not reproduced causes of the user's entire symptom.
Paths are under `clients/cockpit/src/`.

| Priority | Finding and evidence | Smallest useful repair/proof |
|---|---|---|
| P1 | **No shipping local progress loop.** `native_extension.onFrame:588-602` spawns, sizes and refreshes tab runs. `ts_engine.feedShellOutput:1012-1028` drains writes on output, but no shipping `searchPump`/periodic outbound drain exists. `terminal_runtime:128-150,178-193` retains blocked writes/replies; `terminal/session:1241-1269,1297-1335` needs later search slices. | One runtime maintenance scheduler; refused-write recovery with no additional input/output, deep-search completion and occluded-window progress. This is local-only, not an explanation for all remote keyboard failures. |
| P1 | **Input scope is frame/paint-derived.** `native_extension:583-610` updates overlay globals and `input_suspended` during paint/frame; painting also assigns `tab_placement`. `ts_engine.onKey:1122` and `onText:1227` refuse unfocused/suspended input. | Capture current focus/scope at failure. Test modal open/close followed by input before any frame; commit scope synchronously at the model transition, keep painting read-only. The stale-scope consequence needs executable ordering proof. |
| P2 | **Divider capture lacks tab/tree identity.** `ts_engine.routeSplitDrag:1319-1381` stores window/node but applies later movement to that window's currently selected tree. `setFocused:1450-1460` does not clear `split_drag`. | Drag in A, switch to another split tab B before release, then move. Cancel on invalidating transitions or fence to originating stable tab/tree/branch; blur must end capture. |
| P2 | **Failed local pane still admits special keys.** `ts_engine.onKey:1130-1138` checks search/selection but not `acceptsInput` before encoding. `interaction.rememberKey` checks liveness only for ownership recording, not transmission. Text/paste do gate liveness. | Retain failed-spawn presentation operations but assert Enter/control/release produce no PTY write. Add the liveness gate at transmission. |
| Investigation | **Global revision refusals can affect non-positional commands.** `Engine.applyIntent` and current navigation documentation retain the all-intent revision contract. | Ordering tests before any relaxation; preserve positional catalog fences and window epochs. No claim of reproduced lost user commands yet. |

Mid-gesture mouse-mode replacement and repeat-after-focus-change are additional
capture test cases. Ordinary remote pointer/key support now exists; a missing
stress case does not establish that every such gesture is broken.

## 3. Current acceptance gap

The checked-in
[native completion record](../clients/cockpit/docs/NATIVE_COMPLETION_ACCEPTANCE.md)
reports extensive automated checks and earlier live success on a predecessor.
It explicitly says the final composition did not complete interactive acceptance
because the console was locked. Background text was correctly refused. That
record is historical evidence, not proof that this Mac is currently locked or
that the current report has the same cause.

The document names `phux-slogic.4.4` for remaining live acceptance; that ID was not
found in the local Beads database during this investigation. The new recovery
task must carry an actionable current acceptance scope rather than rely on an
unresolvable handoff.

The remaining acceptance oracle is:

```text
actual OS / identified SDK-injected event
  -> shipping extension and TS command translation
  -> focused/captured provider-qualified target
  -> Rust client / FFI / Phux protocol
  -> intended server PTY executes a command
  -> output stream and replica
  -> app terminal presentation
```

An outgoing frame tag is useful contract evidence. It is not server receipt,
shell execution, or proof that the OS supplied the same event as the fixture.
Check decoded target, action, modifiers and text as well as frame kind. For the
end-to-end oracle, use an output token computed from pieces so input echo cannot
satisfy it. Assert absent before Enter, present afterward in both the server
terminal and Cockpit, and absent from another target.

At `6bb68787`, the parent reran `just cockpit-test`: **exit 0, 52/52 steps,
536 passed, 2 skipped**, including 98 shipping-extension tests. The verdict names
this worktree's source root/private cache and Phux **COMPILED AND TESTED**.
Logs from both runs are `cockpit-recovery-baseline.log` and
`cockpit-recovery-current.log` in this session's OpenCode temporary directory.
The docs-only delivery changes no runtime code and claims no live input fix.

## 4. Target architecture

### Ownership, rather than another layer of wrappers

```text
Native SDK / AppKit
  OS events, composition, key windows, clipboard, scheduling, native rendering
                         |
Cockpit SDK adapter
  preserve routed event context; translate effects and terminal presentation
                         |
Cockpit native runtime
  target resolution, gesture ownership, command outcomes, maintenance
  placement/topology transactions, provider lifecycle, persistence
              /                              \
 direct-local adapter                   Phux adapter
 PTY + terminal emulator                Rust client / C ABI / replica
 explicitly ephemeral                  Phux-owned durable execution

TypeScript + .native chrome <---- owned projection / typed requests ----> runtime
                  read-only terminal painter + one geometry calculation
```

This structure builds on existing `terminal_interaction`, `shipping_pointer`,
`workspace_lifecycle`, `durable_creation`, and recovery modules. Deepen and
consolidate them; do not introduce a second framework with competing ownership.

| Fact | Writer | Other layers receive |
|---|---|---|
| OS key window, composition, clipboard completion | Native host | Origin-bearing events |
| Focused view, terminal target, held input | Cockpit runtime | Read-only projection and explicit transitions |
| Palette query, settings draft | TS UI | Committed interaction scope and typed requests |
| Local execution and emulator | Local adapter | Presentation, lifecycle and operation outcomes |
| Phux execution and authoritative operations | Phux, through Rust client/FFI | Correlated acceptance, publication and replica state |
| Client placement and saved attachment evidence | Existing Cockpit topology/recovery | Owned snapshots, never invented restored processes |
| Geometry | Existing compiled-space/projection calculation | Same rectangles for paint, hit test and viewport sizing |

### Small interface; complete invariants

Illustrative, not a new public ABI:

```text
handle(origin, interaction) -> outcome
command(target, request) -> command_outcome
maintain(wake) -> next_wake
snapshot() -> owned UI projection
present(view, geometry) -> frame-scoped presentation
```

Dedicated internal handlers keep this from becoming a giant event switch.
The leverage comes from hiding the rules below, not reducing the number of
exported functions to an arbitrary target.

- **Resolve once.** Use stable view identity, `TerminalRef`, and `ReplicaOwner`
  where generation matters. No fallback to the newly focused pane when an
  asynchronous target has disappeared.
- **Carry origin.** A window, widget/modal scope, and gesture owner are different
  facts. Preserve the SDK's routed information instead of inferring all of them
  from a mutable process-global active-window value.
- **Capture complete gestures.** Press captures the owner; repeat and release
  follow it. Blur, reconnect, close and modal transitions have explicit
  cancellation rules. Special macOS editing transformations need the same rule;
  transformed presses must not create unmatched physical releases.
- **Commit input scope before another event.** Palette/search/settings ownership
  cannot depend on a later render frame. Painting reads application state.
- **Preserve physical key versus committed text.** IME/dead-key text is already
  composed; do not apply modifiers twice. Phux input stays structured and the
  server's terminal modes own its VT encoding.
- **Fence clipboard completion.** Retain source replica and destination
  (terminal versus search), then revalidate completion and cancellation.
- **Return meaningful outcomes.** Distinguish handled/no-op, unavailable,
  stale target, unsupported, queue refusal and I/O failure. Avoid per-keystroke
  UI noise: aggregate actionable state and keep diagnostic counts. Accepted
  queue admission does not mean the server executed the action.
- **Guarantee progress.** Pending PTY input, terminal replies and incremental
  search request bounded future work, including when output is quiet or windows
  are occluded. Cancel wakes after draining. No idle busy loop.
- **Own snapshot data, borrow presentation briefly.** TS receives owned values;
  remote grids cannot outlive their provider's borrow/mutation contract.
- **Do not replay ambiguous input.** Disconnect must not automatically resend a
  command that may already have executed. Creation's existing correlated
  unknown/accepted/publication states are not interchangeable with keystrokes.

Use two concrete adapters. An extensible plugin registry adds no value to the
local/Phux cases being solved here.

### Command preconditions need a measured change

The current navigation contract deliberately uses revision-fenced positional
catalog indices. Keep those fences until replaced with equally strong stable
target validation. The same global revision also gates unrelated commands;
title churn and snapshot delay can reject an action whose target remains valid.

Before changing that contract, test: two commands before snapshot commit;
metadata changes between event and intent; stale/reused window slots; OS closure
already completed. Then separate presentation freshness from command-specific
preconditions. Treat OS lifecycle as idempotent facts, and never weaken positional
safety simply to make a refusal disappear. This explicitly reopens the question
documented in [Navigation seam](../clients/cockpit/docs/NAVIGATION_SEAM.md).

### Durable creation is not complete shared workspace projection

The current release has real spawn/attach/detach operations, pending creation,
coordinator incarnation checks, persisted attachment evidence and restoration.
Preserve that work. The older claim that the FFI has no creation interface is
obsolete.

Existing `phux-l7e5` proposes a further shift: Rust-owned stable shared windows and
layout projection, with Cockpit consuming shared composition while keeping
selection, focus and native placement local. That is an authority migration,
not an input fix. Its ADR must define how current client-persisted placement
migrates and how concurrent clients change shared composition. Do not copy a
window index into the wire and call it stable identity. Keep local scratch
distinct from a shared authoritative tree.

Closing a placement, unsubscribing a replica, and terminating execution remain
different actions. A dev UI restart should preserve Phux execution; direct
local PTYs intentionally do not provide that guarantee.

Current automatic attachment recovery requires matching endpoint, server
incarnation and session; its context resolution currently handles Unix endpoints.
Satellite incarnation is deliberately unresolved by coordinator-only evidence.
Do not generalize local-coordinator restart tests into universal remote/satellite
restoration. Native New Window is also not a Phux shared-window mutation:
`phux-l7e5` proposes mapping Phux windows to Cockpit tabs, a separate concept.

## 5. Prior art and architecture alternatives

Primary sources fetched on 2026-09-09:

| Source | Applicable lesson | Limit |
|---|---|---|
| [Ghostty architecture](https://ghostty.org/docs/about#libghostty) | A native GUI consumes a reusable terminal core; tab/split arrangement is not terminal semantics. | `libghostty-vt` is not Ghostty's complete GUI/input integration. |
| [Hashimoto's Zig patterns talk, 2023](https://mitchellh.com/writing/ghostty-and-useful-zig-patterns) | Separate app runtime, surface, I/O and rendering; exercise implementations through their real consumers. | Build-time alternatives are not runtime local/Phux routing. |
| [Libghostty roadmap, 2025](https://mitchellh.com/writing/libghostty-is-coming) | Reuse a battle-tested terminal implementation and shape interfaces with real consumers. | Historical roadmap, not evidence of today's stable embedding interface. |
| [WezTerm multiplexing](https://wezterm.org/multiplexing.html#multiplexing), [Domain source](https://github.com/wezterm/wezterm/blob/main/mux/src/domain.rs) | Creation belongs to an explicit execution domain; attach and spawn/detach capabilities differ. | Phux already owns coordination; copying another multiplexer would duplicate it. |

"Hashimoto-level" here means locality of reasoning, honest capabilities,
deliberate seams and repeatable behavioral evidence. It is not a language choice
or a promise based on similarity to a respected project.

| Design | Benefit | Cost and decision |
|---|---|---|
| **Thin TS chrome, one deep Zig runtime, existing Rust FFI** | Preserves recent provider/lifecycle fixes, declarative UI and terminal hot path. | Preferred. Must eliminate parallel policy and paint-driven ownership rather than add wrappers. |
| **Canonical Zig app owner with `.native` chrome** | Removes internal TS snapshot/intent protocol and compiler-driven flattening. | First fallback if a repaired TS seam still requires extensive parallel models. Rebind transient UI and revalidate menus, text input, accessibility and windows; do not resurrect the entire old reducer. |
| **Swift/AppKit shell with reusable terminal runtime** | Direct native responder/window integration; credible Ghostty-like arrangement. | Larger integration/automation/packaging surface. Require a narrow prototype demonstrating a concrete host limitation and a better result before migration. |
| **Move remote VT ownership from Rust to Zig** | Could unify replica presentation/search mechanics. | Existing `phux-huj0` experiment. Must measure bootstrap/history/anchors/memory/recovery; not prerequisite to input acceptance. |

The dependency is the pinned owned `phall1/native` fork over `vercel-labs/native`.
The renderer is not implicated by the old early-return defect. Keep packed cells,
CoreText host rendering, terminal emulator and one geometry. Make SDK changes
where context or authoring support is genuinely missing, backed by a minimal SDK
reproducer plus the Cockpit scenario. Do not move Phux policy into the toolkit.

Decision criterion for retaining TS: one provider-aware runtime, explicit target
and scope, read-only paint, one projection contract, no separately maintained
behavioral implementation for tests. If that cannot be achieved without
proliferating glue, compare the supported Zig owner before replacing the toolkit.

### Complexity evidence

No project-native Zig cyclomatic threshold was found. For three switch-free
functions the parent counted source tokens after excluding comments/strings:
`1 + if + for + while + catch + and + or`, with `orelse` reported separately.
This is a reproducible lexical proxy, not a Zig control-flow-graph metric.

| `ts_engine.zig` function | 0.17.0 proxy / `orelse` | 0.18.0 proxy / `orelse` | After this docs-only task |
|---|---|---|---|
| `onKey` | 29 / 1 | 8 / 2 | 8 / 2 |
| `onText` | 6 / 1 | 4 / 2 | 4 / 2 |
| `newTerminal` | 3 / 0 | 5 / 0 | 5 / 0 |

The current release already deepened the first two handlers. The old defect was
in a tiny lookup, so low complexity alone would not have prevented it. Current
navigation documentation separately records ESLint `core.update` at 87 and
compiler restrictions on command-producing helpers. Treat that as historical
measurement until rerun on a touched function, not a threshold pass. Prefer
named policies and a better representation over score-driven extraction.

## 6. Live development while the user uses Cockpit

### Available now

From this worktree root, following [setup](../docs/SETUP.md#cockpit):

```sh
just cockpit-ffi
# Select the intended test server/session with PHUX_SOCKET / PHUX_SESSION.
clients/cockpit/scripts/dev-run.sh --phux --automation --ffi-profile ffi-dev
```

This is **Phux Cockpit (dev)**, with a distinct bundle/process and isolated
Cockpit config, topology state and automation dropbox. The launcher prints its
PID, paths, log and pinned automation CLI invocation. Run that CLI from the dev
home and verify `publisher_pid` before driving input. This is not a claim that
every SDK log or window-state path is isolated.

The current foreground wrapper owns shutdown. Stop only that dev client when
relaunching; use the same dedicated Phux session. Prove stable terminal identity,
same shell instance and surviving shell variable/workload before claiming a
restart-safe feedback loop. Current durable placement restoration exists, but
its real-host round trip must be tested for the exact candidate.

### Two iteration modes

- **Markup:** the SDK has Debug fragment watching and last-good parse recovery.
  Cockpit registers source-relative fragment paths, while its isolated launcher
  runs from the dev home and defaults to ReleaseSafe. `--debug` alone is not
  evidence that watch paths resolve. Provide a Debug source root independent of
  process CWD; prove primary/secondary markup changes with the same PID and
  surviving terminal, including malformed-edit recovery.
- **TS/Zig/FFI logic:** build and relaunch. No implemented state-preserving core
  hot swap was established. `native automate reload` is not evidence of native
  engine replacement. Measure cold, unchanged, TS-only, Zig-only and FFI build
  times before adding a serialized watcher. A failed build must leave the active
  candidate intact; do not restart on every save while the person is typing.

### The useful missing tool: mark this problem

Extend existing launcher/automation scripts with a timestamped capture operation:
user observation; source/dirty identity; SDK/FFI/server provenance; executable
hash; app and publisher PID; focused window/view/provider/generation; interaction
scope; recent event kinds/outcomes and queue/refusal counters. Exclude terminal,
key and clipboard payloads by default; allow a deliberate content capture for a
specific reproduction. Retain per-run logs instead of overwriting one `app.log`.

The loop is: **use -> mark -> reproduce through shipping hooks -> fix -> validate
-> relaunch -> repeat with the user**. One serial driver owns activation and
automation; passive diagnostics can continue during human use. Refuse a second
dev instance before restaging anything. An attach-to-existing-dev-PID smoke mode
would avoid launching a competing client; it is proposed, not currently present.

## 7. Acceptance and rollout

### Test surfaces

1. **Shipping contract tests:** extension, native commands, TS intent translation,
   provider output, asynchronous completions and real maintenance scheduling.
   Use deterministic timing where ordering matters. Do not supply a test-only
   frame pump that the app never requests.
2. **Hermetic packaged round trip:** same-checkout FFI and compatible isolated
   server on a unique socket/session, with authoritative PTY output assertions.
   The existing ambient-server structural smoke is not that harness.
3. **Real macOS ingress:** actual keyboard, key equivalents, focus, IME/dead keys,
   clipboard, accessibility and activation. SDK widget injection starts after
   part of this path. CPU-reference screenshots cannot validate CoreText glyphs.

For each bug fix, observe its behavioral regression fail on the actual broken
implementation and pass after the fix. Follow the current
[mutation testing policy](../docs/TESTING_MUTATIONS.md). Permanent `.guard`
patches/markers and the old runner were retired in `6319330a`; do not restore
that machinery as part of recovery.

### Minimum workflow matrix

| Workflow | Evidence |
|---|---|
| Attach; text then Enter | Computed output absent before Enter, present in intended server PTY and app afterward. |
| Create tab/split/window then return | Exact published owner and target; correct shell responds; no input in another pane. |
| Actual secondary-window command | Source window's command context is preserved, even during snapshot churn. |
| Press/repeat/release with focus changes | Full gesture reaches original owner or explicitly cancels; no orphan release. |
| Search and menu/key Paste | Visible query changes, zero shell paste; delayed completion remains fenced. |
| Open/close modal then immediate input before frame | Scope changes synchronously; terminal cannot consume overlay input or stay suspended afterward. |
| Resize/font/split; pointer/wheel | Same resolved geometry drives intended viewport/focus/selection and remote PTY size policy. |
| Quiet child after refused write; deep search | Pending work progresses without more input/output and when occluded; no idle spin. |
| Title churn, burst commands, recycled windows | Valid targets work; stale positional actions refuse explicitly; OS close converges. |
| Disconnect/reconnect and delayed clipboard | Generation ownership survives/cancels correctly; no replay into replacement. |
| Close/relaunch UI | Phux shell/process and durable identity survive; placement restores according to current contract. |
| Maximum inventory | Bounded snapshot/page data, deliberate label elision, no engine-unavailable overflow. |

Use `just cockpit-test`, then actual packaged acceptance for consequential
changes. Test local and Phux configurations explicitly for shared routing/SDK
work. FFI/protocol changes expand to relevant Rust/ABI/server gates. Final
candidate uses production optimization and `ffi-release`, not only Debug mocks.

Measure latency and work separately: input round trip, frame work, queued bytes,
idle wakeups, memory and rebuild/relaunch time under quiet and sustained output,
split churn and reconnect. Record sample counts and existing baselines; this
design invents no speedup or arbitrary latency threshold.

### Migration discipline

- Extract reusable policy from the retained reducer into the existing runtime
  modules, then make retained tests use it. Remove duplicate app-only dispatch
  after shipping-path coverage exists; preserve terminal/provider/geometry tests.
- Each slice leaves a usable app. Unknown/unavailable results are visible,
  not silently converted into another execution domain or claimed as ready.
- Measure touched functions before and after with a consistent tool/counting
  convention. Complexity is a diagnostic, not a substitute for correct ownership.
- Keep one writer; independent read-only review checks each consequential seam.
  Child reports are evidence, not automatic acceptance.
- Record enduring application/SDK and shared-workspace decisions in ADRs after
  the small experiments settle them. This research remains a proposal.

### Ordered work, already tracked

| Order | Beads | Deliverable and exit evidence |
|---|---|---|
| First | `phux-mz6e.1` | Provenance-matched reproduction and hermetic packaged command round trip. Inspect the actual 0.18.0 focus/suspension/target before diagnosing its symptom. |
| Independent narrow repair | `phux-mz6e.6` | Bounded local outbound/reply/search maintenance, with quiet-child and occlusion liveness proofs. |
| Independent narrow repair | `phux-mz6e.4` | Synchronous modal scope, stable divider capture and failed-local-pane liveness gates, with adversarial ordering tests. |
| Support the first loop | `phux-mz6e.2` | Identity-bound problem capture, retained run logs, serial launcher preflight and proved Debug markup reload. |
| Evidence before interface change | `phux-mz6e.7` | Burst-command/title/OS-close ordering cases; command-specific preconditions only if justified, preserving positional catalog safety. |
| Consolidate after these behaviors are pinned | `phux-mz6e.3` | One runtime implementation shared by app/tests; read-only paint, explicit context/outcomes, remove redundant policies. |
| Separate architecture project | Existing `phux-l7e5` | Shared workspace identity/projection authority and migration of current client-local composition. Not needed for today's durable terminal creation. |
| Conditional measured experiment | Existing `phux-huj0` | Rust-replica versus Zig-VT ownership tradeoff, independently reviewed before changing FFI responsibility. |

The initial `phux-mz6e.5` navigation/search-visibility task was closed as
superseded by `aee36811`, not claimed as newly implemented. The other newly
created tasks were rewritten against current source; old descriptions must not
drive duplicate fixes. The correctness repairs can proceed while live acceptance
is scheduled. Framework/FFI replacement is not on their critical path.
