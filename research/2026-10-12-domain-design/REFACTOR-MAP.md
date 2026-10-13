---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Refactor map: the one-sweep implementation

**TL;DR.** The whole domain change lands as one integration branch built by twelve lanes with non-overlapping file scopes. A single-writer spine freezes the shared types first: core records, protocol variants, the arrangement document, and the FFI header. Server, client libraries, docs, and web then run in parallel; FFI, CLI/MCP, and TUI follow the client libraries; Cockpit and Desktop follow the frozen header. Twenty-two decisions below settle every question the code survey raised, so no lane has to stop and ask. Every claim cites the tree as of `7235d52d`.

Conforms to [CONTRACTS.md](CONTRACTS.md) and [MIGRATION.md](MIGRATION.md), including draft 3's Inbox, bulk move, undo, and SQLite store. Where this map and the contracts disagree, the contracts win and this map is fixed.

## 01. Size of the change

| Symbol | Hits | Files |
|---|---|---|
| `SessionId` | 798 | 143 |
| `AgentSession` | 765 | 116 |
| `WindowId` | 550 | 108 |
| `SessionSnapshot` | 491 | 106 |
| `keep_empty` | 293 | 65 |
| `WindowInfo` / `WindowSnapshot` | 232 | 62 |
| `phux.tui.layout` | 39 | 21 |

Roughly 300 files change. Most `SessionId` and `AgentSession` hits survive untouched (decision R01). The real rewrites concentrate in about 40 files, listed per lane in §05.

## 02. Decisions the survey forced

These are binding for every lane. Each one closes a question a lane would otherwise stop on.

| ID | Decision | Why |
|---|---|---|
| R01 | Core keeps its slotmap `SessionId` and `ResourceId` as in-memory live handles. Records gain `key: SessionKey` / `key: ResourceKey`; `Registry` gains `HashMap<Key, Id>` indices. Only `WindowId` is deleted from core. | About 35 server files and 96 `CoreResourceId` sites use the handles. CONTRACTS §01 already separates keys from handles. |
| R02 | phux-core does not depend on phux-protocol. Core owns its own `Command`, `Refusal`, `InstanceId`; the server maps between them. Keys are minted in the server (`getrandom`, already a dependency) and passed into `apply` through a `FreshKeys` argument. | Keeps core pure and IO-free (`crates/phux-core/src/lib.rs:3-6`). |
| R03 | `LayoutNode<L>` becomes generic. Persisted and wire arrangements use `EntryRef { entry, resource: ResourceKey }` leaves. Clients edit in handle space; `phux-client-runtime` translates key to handle on read and handle to key on `ArrangementReplace`. | Handles are reused after restart (`crates/phux-client-core/src/layout/identity.rs:1-56` hashes live IDs); keys are not. |
| R04 | Group and entry IDs are random 16 bytes, minted by whoever creates them. The SHA-256-of-seed-terminal scheme is deleted. | Same reason as R03. |
| R05 | Resource-to-session link: `ResourceInfo` gains `session: SessionId` (wire handle) in its extension block. Every client join goes through one helper, `phux_client::resource::session_of(snapshot, &ResourceId)`. About 14 joins move onto it. | Today every join goes resource, then `window_id`, then `WindowInfo.session_id` (`crates/phux-protocol/src/wire/info.rs:246-248`). |
| R06 | Legacy `GetState` keeps `SessionSnapshot.windows`, synthesized as exactly one `WindowInfo` per non-empty session, named `window-0`, its wire `WindowId` interned from the `SessionId` and carried in the upgrade blob. `keep_empty` on the wire equals `lifetime == Persistent`. Byte-identical goldens prove it. | Old TUIs, mobile, and released desktop builds read it. The server already has exactly one window per session (`crates/phux-server/src/state/session_table.rs:172-179`). |
| R07 | Scope verbs `ORGANIZE = 0x40` and `TERMINATE = 0x80` (`crates/phux-protocol/src/kinds.rs:55-111`). The scope schema version bumps. Legacy commands keep exactly their current verb requirements, so no existing grant loses authority. A schema-v1 grant decodes with `SIGNAL` implying `TERMINATE` and `CREATE` implying `ORGANIZE`, so old registries reach the typed equivalents of what they could already do. `*` covers all eight. | `workload-auth.md` §5 requires a schema bump for new verbs; moving `KILL_*` off `SIGNAL` would silently de-authorize existing grants. |
| R08 | `CommandResult::Refused` (tag 0x03) and `Accepted` are sent only in reply to organization commands. Translated legacy writes and old commands never receive them. | Older decoders reject the whole frame on an unknown result tag (`crates/phux-protocol/src/wire/frame/command_codec.rs:764-780`). |
| R09 | `ActorRef` gains `principal` and `on_behalf_of` as optional trailing fields that encode to zero bytes when absent. Existing goldens stay byte-identical. | Shared by `METADATA_CHANGED` and the event stamp; `sub!` ignores trailing bytes (`decode.rs:40-45`). |
| R10 | Org store is SQLite through `rusqlite` with the `bundled` feature, WAL mode, `synchronous=FULL`. It is the one new dependency; the justification paragraph is CONTRACTS §08. Commits run inline in `ServerState::commit` on the runtime thread; the input lane is a separate OS thread and is unaffected. A perf gate pins commit latency. | No SQLite binding exists in the tree (`Cargo.lock`). Draft 3 requires indexed membership, intake queries, and durable receipts. |
| R11 | `ServerConfig` gains `org_store: OrgStoreLocation { Path(PathBuf), Memory }`. In-process tests default to `Memory`. `ServerGuard` and `phux_cmd` set a per-test `XDG_STATE_HOME`. This lands before the store. | `ServerGuard` sets no `XDG_STATE_HOME` (`crates/phux/tests/common/server_guard.rs:207-230`); every e2e server would share the developer's real dev store. |
| R12 | `ProcIdentity { pid, pgid, start_raw, boot_id }`. `start_raw` is the platform's raw start time from `proc_query::process_start_time` (`crates/phux-server/src/proc_query.rs:60`), not `start_ms`. `boot_id` is `/proc/sys/kernel/random/boot_id` on Linux and `kern.boottime` on macOS; a different boot means the pid is gone. `pgid` is asserted equal to pid once with `getpgid`. | `start_ms` is not comparable across server restarts on Linux (`crates/phux-core/src/process.rs:59-60`). |
| R13 | Server lifetime: exit when no activation is live and no client is connected, after the existing idle backstop. Auto-spawned ephemeral servers keep the `idle_exit_e2e` contract. | Replaces the zero-session exit (`crates/phux-server/src/state/bindings.rs:192-196`) without stranding servers. |
| R14 | Sessions keep an advisory `recent: Option<ResourceKey>`: the resource that last received human input. It is not focus and never moves a client. `send-keys` without a pane target uses it, else the first leaf of the first group. | The server no longer has per-session focus; `send_keys.rs:124-143` needs a source. |
| R15 | TUI attach policy: the TUI detaches only when its session is deleted or the user detaches. An arrangement emptied by close-view while members run shows "N running, not arranged" with a reopen action. | After close-view, an empty arrangement no longer means an empty session (`crates/phux-tui/src/attach/server_frame/handler.rs:1481-1540`). |
| R16 | `phux archive import` never launches. `restore_archive` splits into `import_archive` and `relaunch_imported`; `project open` and recipes call both explicitly. | MIGRATION §07, without breaking `crates/phux/src/commands/project.rs:17` and `project/recipe.rs:16`. |
| R17 | Only `phux.tui.layout/v1/<session-id>` is translated to and from the arrangement. Named projections (`<prefix>.layout/v1/N`, ADR-0129) and third-party layout keys stay opaque L3 metadata, untouched. | The spec left their fate open; leaving them alone is the conservative call. |
| R18 | C ABI: bump `PHUX_CLIENT_ABI_VERSION` to 3 (`crates/phux-client-ffi/include/phux/client.h:12`). `PhuxSessionInfo` is frozen; a new sized `PhuxSessionRecord` carries key, revision, lifetime, and counts. Rename every `PhuxWorkspace*` to `PhuxArrangement*`. Wire the `tests/*_abi.c` static-assert files into `just cockpit-test`. | The `*_abi.c` checks run nowhere today. |
| R19 | Desktop and web stop auto-creating `default`. Both open the owner's Inbox session (draft 3), which always exists. | `clients/desktop/scripts/server.ts:144-158`, `clients/phux-web/src/session.rs:1313-1320`. |
| R21 | An arrangement entry targets `EntryTarget::Member(ResourceKey)` or `EntryTarget::Satellite { host, id }`. Only member entries are checked against session membership. Satellite entries are presentation only: placeholders when unresolved, never an attach grant. | The TUI mixes local and satellite panes in one window; translated layouts must not be refused. |
| R22 | `GetOrganization` and `ArrangementGet` require `INVENTORY`. The default delegated agent grant is `INVENTORY`, `OBSERVE`, `INPUT` on its own session, without `SIGNAL`, because `SIGNAL` still authorizes legacy kills (R07). | ADR-0161 and ADR-0163. |
| R20 | The root help line cap (`crates/phux/src/help_inventory.rs:158`, exactly at 93 today) rises to 97 in the same commit that adds `rm`, `relaunch`, `restore`, and `mv`, with the reason in the test. `move-pane` and `workspace` become hidden aliases. | Four new user-facing verbs are worth four lines; hiding them would hurt discovery. |

## 03. Phases

```text
Phase A  spine (one writer) ───────────────────────────────► freeze tag "domain-types"
           L0 test isolation · L1 core · L2 protocol types · ArrangementDoc rename · C header draft
Phase B  parallel ─────────────────────────────────────────►
           L3 server      L4 client libs      L10 web      L11 docs/spec/ADRs
Phase C  parallel, after L4 ───────────────────────────────►
           L5 FFI         L6 CLI + MCP        L7 TUI
Phase D  parallel, after L5 header and DTOs frozen ────────►
           L8 Cockpit     L9 Desktop
Phase D' after L3 ─────────────────────────────────────────►
           L12 actors: principal registry, delegated agent credentials, human lease precedence, lease epochs
Phase E  integration (one writer) ─────────────────────────► just ci-full + client gates, then merge
```

Branch: `feat/domain-model` off `main`, in its own worktree. Each lane works in `.worktrees/dm-<lane>` on `dm/<lane>` and rebases onto the integration branch. Lanes never share a cargo target directory; each copies a deps-only base. Before integrating any lane, verify the primary checkout's HEAD and `git status` are untouched.

## 04. Phase A: the spine

One writer, in this order. The output is the frozen interface every lane builds against.

1. **L0 test isolation.**
   - `crates/phux/tests/common/server_guard.rs:207-230` and the `phux_cmd` helper get a per-test `XDG_STATE_HOME` and `XDG_CONFIG_HOME`, copying `fleet/production_state_guard.rs`.
   - `ServerConfig` (`crates/phux-server/src/runtime/mod.rs:83-187`) gets `org_store: OrgStoreLocation`, defaulting to `Memory` in test constructors.
   - Gate: `just test` green, unchanged behavior.
2. **L1 core types** (`crates/phux-core`):
   - New `keys.rs`: `AuthorityId`, `SessionKey`, `ResourceKey`, `ArrangementKey`, `WorkspaceKey`, `OperationId`, `InstanceId` (16 bytes, base32 serde, shortest-unique-prefix display), plus `ActivationId`, `Revision`, `LeaseEpoch` (u64).
   - New `activation.rs`: `Activation`, `ExecState`, `Outcome` (wraps `ExitOutcome`, adds `Unobserved`), `ExitPolicy`, `SessionLifetime`, `Stamp`, `ProcIdentity` (R12).
   - New `arrangement.rs`: `LayoutNode<L>`, `SplitDir`, `LayoutError`, `split`, `remove`, `leaves`, lifted from `window.rs:34-158,238-286`; plus `Arrangement`, `Group`, `EntryRef`, `validate(members)`.
   - New `org.rs`: `Org` snapshot, `Command`, `Refusal`, `Transition`, `Effect`, `Authority { id, inbox_session }`, `Receipt { inverse, revisions }`.
   - `session.rs`, `resource.rs`, `registry.rs` rewritten per §05 L1. `window.rs` deleted.
   - `apply.rs` lands in phase B (lane L1b, below); the spine freezes its signature only.
3. **L2 protocol types** (`crates/phux-protocol`), encodings included:
   - `ids.rs`: key types through a `key16!` macro; `OperationId` aliases the existing `IdempotencyKey` (`ids.rs:159`).
   - `caps.rs:1058-1078`: `ServerFeatureExt::ORGANIZATION = 0x10` (0x8 is `AGENT_STREAM_RESUME`, ADR-0159), plus `wire_name()` and `ALL`.
   - `wire/frame/command.rs`, `mod.rs`, `command_codec.rs`, `field.rs`: command tags `0x1e`-`0x2b` (§05 L2); event tags `0x10` onward (`0x0f` is main's `input_activity`); result tag `0x03`; value tags `0x06` onward; names capped at 256 bytes, arrangements at 64 KiB.
   - `payload.rs`, `codec.rs`: R09.
   - `info.rs`: `ResourceInfo.session` (R05) and `SessionInfo` lifetime in extension fields 3 and 4.
   - `kinds.rs`, `scope.rs`, `kinds/samples.rs`: R07, new `Subject` variants, rules, `MethodSpec` with the first `FeaturesExt` gate.
   - Spec text that `tests/kinds_table.rs` parses changes in the same commit: `docs/spec/workload-auth.md` §5 and §6, `L1.md` §5.1 and §7.1, `appendix-reserved.md` §2.
   - Gate: phux-protocol tests green; the 78 existing insta goldens byte-identical; new goldens for every new command, result, and event.
4. **Client-core rename** (`crates/phux-client-core`): `layout::Workspace` becomes `ArrangementDoc`, `WindowState` becomes `Group` with random IDs (R04). The legacy CBOR codec moves under `layout::legacy`. Touches about 27 TUI files mechanically; do it here so later lanes start from the new names.
5. **C header draft**: `client.h` with R18 applied and stub exports, so Cockpit and Desktop can start against a stable surface.

Freeze: tag the integration branch `domain-types`. After the freeze, changing a shared type is a spine change, made by the integrator and announced to every lane.

## 05. Lanes

Each lane lists its scope, the changes that matter, the tests whose meaning changes, and its own gate. Mechanical fixture churn is not listed; it follows the types.

### L1b Core rules

Scope: `crates/phux-core/src/apply.rs`, `crates/phux-core/tests/**`.

- `apply(org, cmd, actor, now, fresh) -> Result<(Transition, Vec<Effect>), Refusal>` implementing every command in CONTRACTS §04, including `ResourceMoveMany`, `InboxResolve`, and `OrganizationUndo`.
- Invariants enforced here: one session per resource; ephemeral sessions are never empty after commit; the designated Inbox session is never ephemeral or deleted; `SessionDelete` refuses `NotEmpty`; remove and relaunch refuse while an activation is current or `Lost`; revision checks yield `Conflict`; exit with `ExitPolicy::Remove` yields a remove transition; undo applies only when recorded resulting revisions match.
- Tests rewritten: `tests/domain/registry.rs` (`window_and_session_removal_cascade…` `:78`, `removing_terminals_collapses_layout_and_rolls_focus` `:59`, the proptest `:232-291`); `tests/domain/layout.rs` ports to `LayoutNode<u32>`.
- New: a command-sequence proptest checking every invariant after every step.
- Gate: `cargo nextest run -p phux-core`.

### L3 Server

Scope: `crates/phux-server/**`, `crates/phux-server-testkit/**`.

State layer:

- `state/session_table.rs`: drop `window_last_cwd` (`:33,203-219`), `active_window_of`, keep-empty ledgers (`:34-38,228-245`); `seed()` returns `(SessionId, ResourceId)`; `add_pane` resolves through `resource.session`.
- `state/sessions.rs:98-144`: delete `should_replace_last_shell`, `is_sole_session_terminal`, `session_terminal_ids`.
- `state/reap.rs`: delete `reap_window_if_empty` (`:109-124`), keep-empty logic (`:170-231`), `take_killed_sessions` (`:164`). `reap_terminal` removes the resource through `commit`; an ephemeral session emptied by it is deleted in the same transaction.
- `state/bindings.rs:189-196`: R13.
- `state/snapshot.rs:14-215`: R06 synthesis; `workspace_revision` (`:225+`) uses the org revision.
- `state/id_space.rs:26-27,166-202`, `state/wire_ids.rs`: window bridge keyed by `SessionId`.
- `state/upgrade_blob.rs` and `upgrade/blob.rs`: additive `#[serde(default)]` fields (session key and lifetime; resource key, activation, `ProcIdentity`, exit policy). The reader still accepts `BLOB_VERSION = 1` with windows and flattens them into members and an arrangement. Keep `dangling_window_id_does_not_mutate_state` and `layout_leaf_must_name_a_pane_in_the_blob` for the v1 reader.
- `policy/enforce.rs:435-439`: `Group` selectors resolve through `resource.session`, re-evaluated in the same transaction as a move.
- New `state/org.rs`: `ServerState::commit(cmd, actor) -> Result<Vec<Effect>, Refusal>`, the only mutation path: `apply`, then store transaction (records, receipt, journal sequence), then in-memory mirror, then ack, then publish. A failed store write leaves memory unchanged.

Runtime layer:

- `runtime/client.rs:1206-1210,1283-1320`: delete the respawn loop and `replacement_shell_command`. On child EOF, commit `ActivationChanged(Completed, outcome)`; `ExitPolicy` replaces `retain_exited_pane` (`:1213`), with `state/retained.rs` (ADR-0124) as the retention mechanism.
- `runtime/client.rs:4311-4435,4753-4860`: intercepted keys translate into `commit(SessionCreate | SessionRename | SessionSetLifetime)` attributed to the writer. `keep_empty: true` and `empty: true` mean Persistent; a bare create means Ephemeral. Legacy broadcasts continue. Delete the session-count exit at `:4416-4431`.
- `runtime/client.rs`: new handlers for every organization command in CONTRACTS §04, all through `commit`; long operations return `Accepted` and publish events.
- `runtime/commands.rs:258-412,3026-3097`: session creation and spawn go through `commit` plus a launch effect. `:2820-2915`: drop `preserve_keep_empty`.
- `runtime/committed_close.rs:52-100`: deleting an ephemeral session detaches its clients.
- `runtime/attach.rs:1913-1995`: membership move through `commit(ResourceMove)`; `:1616-1750`: a bare create-if-missing is Ephemeral; last-window cwd becomes session cwd.
- `runtime/mod.rs:700-735`: startup order is open store, first-start import (MIGRATION §07, designate Inbox, mark imported sessions), resume from the upgrade blob, then reconcile, then seed.
- `resource/mod.rs:241-262` and `resource/terminal/io.rs:413-440`: delete `ReplaceChild` and `ReplacementCommand`. Keep `reset_for_replacement` and `install_replacement_pty` if relaunch reuses them; otherwise delete.
- `resource/terminal/mod.rs:843`, `construct.rs:314`: `TerminalActorBundle` carries `proc: Option<ProcIdentity>`.
- `autosave.rs`: becomes the export schedule; `write_atomic` (`:242`) stays for archive export.

New files: `org_store.rs` (trait, `SqliteOrgStore` at `phux_config::instance::state_dir()/org/v1.sqlite3`, `MemOrgStore`; schema migrations; receipt pruning at 24 hours or 10,000; tombstones at 30 days), `reconcile.rs` (pure decision function plus syscall shim, R12), `state/legacy_snapshot.rs` (R06, isolated for later deletion).

Tests whose meaning changes:

- `tests/lifecycle/keep_empty.rs` (all six) become lifetime tests.
- `tests/lifecycle/server_lifecycle.rs:122,146,157`; `state/upgrade_blob.rs:1450`.
- `tests/attach/attach_snapshot.rs:119` (`last_shell_eof_keeps_the_terminal_live`) inverts.
- `tests/lifecycle/retain_on_exit.rs:423,459`.
- `tests/protocol/command_dispatch.rs:502,710-753`.
- `state/mod.rs:815,862,876,890,913`; `runtime/mod.rs:3002`; `runtime/client.rs:6928,6972`; `policy/tests.rs:1848`.
- `tests/metadata/event_journal.rs:847` still passes through translation.
- Remove the sibling-pane pins that existed only to dodge respawn: `tests/terminal/kip_roundtrip.rs:14,164`, `tests/agents/program_status.rs:669`, `tests/metadata/agent_events.rs:61`.

New tests:

- Crash safety: SIGKILL the server mid-session and restart it. Dead shells show `Unobserved`, a `nohup` child shows `Lost`, nothing relaunches, and a second restart changes nothing.
- A store write failure leaves memory unchanged.
- The upgrade path from a v1 blob.
- The scale proof from PLAN stage 2.

Gate: `cargo nextest run -p phux-server`, plus the perf gate on commit latency.

### L4 Client libraries

Scope: `crates/phux-client/**`, `crates/phux-client-core/**` (after the spine rename), `crates/phux-client-runtime/**`.

- `phux-client/src/resource.rs`: `session_of` (R05); every window join moves onto it (`spatial.rs:780-795`, `resource/show.rs:114-118`, `pane_move.rs:472-490`, `spawn.rs:172-200`, and others).
- `phux-client/src/kill.rs`: delete `clear_session_keep_empty` (`:285-297`), `kill_empty_session` (`:541-552`), `KillError::SessionKept`. `kill_whole_session` (`:555-567`) becomes the workflow: snapshot members and revision; terminate each; wait on `ActivationChanged` (generalize `await_reaped` `:486-514`); remove each; delete the session with the expected revision; per-target results. A legacy path remains while `ORGANIZATION` is absent.
- `phux-client/src/session.rs:266-482`: `SessionCreate` and `SessionRename`; `OperationId` replaces the nonce read-back except on the legacy path.
- `phux-client/src/layout_ops.rs`: `ArrangementGet` plus `ArrangementReplace { expected }`, a real compare-and-swap retried on `Conflict`; the pure `apply_mutation` (`:356-560`) survives.
- `phux-client/src/selector.rs`: `Window` becomes `Group`; `name:N.M` resolves against the arrangement (depth-first leaf order); `^ab12cd` key prefix; `state.rs:476-520` fetches the arrangement when needed.
- `phux-client/src/session_list.rs`, `phux-core::session_list::SessionJson`: lifetime, member count, state counts; `ls` schema v4.
- `phux-client/src/send_keys.rs:124-143`: R14.
- `phux-client/src/testkit.rs:597-842`: responders for every organization command with revision and conflict injection.
- `phux-client-core`: new `org_ops.rs` (sans-IO operation journal on the `InputReplayJournal` pattern; pure kill-workflow planner); `rename.rs` uses `SessionRename`; delete `organization.rs:170-193` (no callers).
- `phux-client-runtime/src/control/topology.rs`: delete `WindowDescriptor` and pane window fields; add keys, revisions, lifetime, members, arrangement, exec state, handle-key maps.
- New `phux-client-runtime/src/control/organization.rs`: replica seeded by `GetOrganization` (paged), applied from journal events by sequence, refetched on `JournalGap`; pending `OperationId`s retried with the same ID after reconnect; past the horizon, reconcile and report unknown; owner-local Inbox projection.
- `control/responses.rs`, `roster.rs`, `frames.rs`, `events.rs`, `commands.rs:343-355`, `runtime.rs`: route organization events and expose the commands, including the kill workflow, bulk move, and undo.
- Gate: `cargo nextest run -p phux-client -p phux-client-core -p phux-client-runtime`.

### L5 FFI

Scope: `crates/phux-client-ffi/**`.

- `projection/topology.rs:14-89`: lossless projection.
- `c/workspace/*` becomes `c/arrangement/*`: `SharedArrangement` over `ArrangementGet` and `ArrangementReplace`; the fallback catalog (`model.rs:96-116`) goes away.
- `c/session_query.rs`, `session_create.rs`, `session_rename.rs`, `client.rs`, `operations/close_resource.rs`: lifetime, typed commands, terminate instead of `CloseTabResources`.
- New `c/organization.rs`: move, bulk move, terminate, remove, relaunch, delete, undo, Inbox resolve, and the kill-session workflow, each with per-target results.
- `include/phux/client.h`: R18. `tests/*_abi.c` updated and wired into `just cockpit-test`.
- `uniffi/wire/types.rs:410-480`, `napi/dto.rs:110-176`: lossless records.
- `examples/cockpit_fixture.rs`, `workspace_fixtures.rs`, and siblings: rewrite the generators; regenerate the committed Cockpit `.bin` fixtures.
- Gate: `cargo nextest run -p phux-client-ffi`, `just binding-uniffi-test`, `just cockpit-ffi`.

### L6 CLI and MCP

Scope: `crates/phux/**` (except `tests/common`), `crates/phux-mcp/**`, `crates/phux-config/src/{plugin.rs,plugin/**,settings/**}`, `examples/agents/**`, `examples/plugins/**`, `.agents/skills/**`, `docs/reference/**` (generated).

- `commands/mod.rs`: `new --ephemeral`; `--empty` reworded; `Archive { Export, Import }` with hidden `workspace` alias; `project inspect`; new `rm`, `relaunch`, `restore`, `mv` (with `move-pane` as a hidden alias); `rename` accepts resources; `--autosave` documented as deprecated export schedule.
- `commands/workspace.rs` splits: git helpers move to `commands/git.rs` (imported by `worktree.rs:17`, `project.rs:115`); inspect moves to `project/inspect.rs`.
- `commands/workspace/archive*` moves to `commands/archive/`; `snapshot.rs` builds from arrangement groups; R16 split.
- `commands/service.rs:344-400`: wrapper scripts call `phux archive export`; installed wrappers keep working through the hidden alias until `service reconcile` rewrites them.
- `commands/new.rs`, `attach.rs:108,1029-1045`: lifetime per MIGRATION §04.
- `commands/kill.rs:140-230`: confirmation and per-target result output.
- `commands/ls.rs:290-455`: lifetime, members, state counts.
- `deprecations.rs:87`: one row for `workspace`, exemplified by `workspace inspect .`.
- `help_inventory.rs:158`: R20.
- `tests/cli_no_raw_connections.rs:49-51`: allowlist path follows the archive move.
- MCP `tool_table.rs`, `tools.rs`, `cli_tools.rs`, `plugin_tools.rs`, `goldens.rs`: `phux_archive`, `phux_plugin_profile`, `phux_rm`, `phux_relaunch`, `phux_new { lifetime }`, `phux_kill` returning per-target results; new method names registered in `kinds` (phase A) so `method_facts()` resolves.
- Plugin manifests: `[[workspaces]]` becomes `[[profiles]]`, the old key accepted as an alias for one release.
- Regenerate: `just docs-gen` (CLI reference, parity, deprecations, files); MCP goldens including `tests/golden/ls.json`.
- Tests whose meaning changes: `workspace_archive_e2e.rs` (`:488` now requires an explicit relaunch), `autosave_e2e.rs:95,150`, `empty_session_e2e.rs:52-110`, `idle_exit_e2e.rs`, `kill_server_e2e.rs`, `retain_on_exit_e2e.rs`, `first_five_minutes_e2e.rs`, `failure_ux_e2e.rs:437`, `layout_conformance.rs:34,344,498`, `quic_restore_e2e.rs`, `spatial_e2e.rs:411`, `tests/workspace/*` (move to `project inspect`).
- Gate: `cargo nextest run -p phux -p phux-mcp -p phux-config`, `just docs-check`.

### L7 TUI

Scope: `crates/phux-tui/**`, `crates/phux-config/src/{vocab.rs,default.toml}`.

- `input_dispatch/run_action.rs`: new `close_view` (fold the leaf from the arrangement, no kill); `kill_active_window` (`:659-685`) opens a confirm naming the count and any resource also arranged elsewhere, and kills only on commit.
- New `render/overlay/kill_confirm.rs`, modelled on `paste_confirm.rs`.
- `context_menu.rs:49,70`, `action_registry.rs:120-226`: "Close view" and "Kill" as separate entries; `phux-config/src/vocab.rs` gains `close-view`.
- `server_frame/handler.rs:1481-1540`: R15. `:76-500,997,1590-1604`: lifetime replaces keep-empty. `:1115,2185-2280`, `driver/loop_state/peers.rs:140-180`, `input_dispatch/effects.rs:290-310`, `driver/subscriptions.rs`, `bootstrap.rs`, `headless.rs`: arrangement get, replace, and subscribe replace the layout metadata key.
- `server_frame/engine_route.rs:110-114`, `sidebar_zones.rs:125`, `fleet.rs:383-386`, `inventory.rs:148-180`: `session_of`.
- `driver/loop_state/paint.rs:153-160`, `attach/paint.rs:346-380`: empty and not-arranged states.
- Insta snapshots to review and accept: session picker (window counts), palette over sidebar, select-list overlay, sidebar sectioned layouts.
- Tests whose meaning changes: `tests_actions.rs:240-360`; `server_frame/tests.rs:1652,2201,2302,2362`; `driver/loop_state_tests.rs:251`; `paint.rs:1042`.
- Gate: `cargo nextest run -p phux-tui`.

### L8 Cockpit

Scope: `clients/cockpit/**`.

- `providers/phux/workspace_bridge.zig`, `host.zig` (`:675-888,1810-1888,1974,2710`), `provider.zig:850-855`, `operation_types.zig:7`: the renamed C API, lifetime instead of `keep_empty`, terminate instead of `phux_client_queue_close_tab_resources`.
- `cockpit/workspace_lifecycle.zig:93-115`, `phux_support.zig:152-155`, `native/new_session_runtime.zig:44-47`: lifetime.
- Menus (`command-catalog.ts:17`, `native_extension.zig:3711,3730`, `tests/shipping_commands.zig:12`): "Close Pane" and "Close Tab" become "Kill…", plus "Close view".
- Leave Cockpit's own `window_count` (OS windows, `cockpit/topology.zig`, `model.zig`) alone.
- `tests/everyday-remote/{probe.c,support.h}`, `src/empty_session_window_tests.zig`, `close_runtime_tests.zig:165,237`, `close_resource_tests.zig:12`, `multi_coordinator_tests.zig`.
- Compare failing test names against a same-depth `main` baseline; startup and extension fixtures fail at long worktree paths regardless.
- Gate: `just cockpit-test`.

### L9 Desktop

Scope: `clients/desktop/**`.

- Regenerate `native/generated/index.d.ts` after L5's napi DTOs (needs macOS Metal and the GPUIX bootstrap).
- `src/workspace/session-cli.ts:11-13`, `scripts/server.ts:116-158`, `scripts/start-desktop.ts:46-83`, `src/app.tsx:93-135,1775-2133`: runtime kill workflow with per-target results instead of shelling out; explicit lifetime on create; R19.
- `src/shell/sidebar.tsx:367`: group or resource name instead of `windowName`.
- `tests/model/{bridge,workspace-lifecycle,session-cli}.test.ts`, `tests/native/presentation.rs:11,68-75`.
- Gate: `just desktop-check`, `just desktop-native-build`, `desktop-native-client-test`.

### L10 Web

Scope: `clients/phux-web/**`, `docs/site/src/lib/phux-web/**`, `scripts/ci/web-browser.py`.

- Rename `Session` to `Connection` (`src/session.rs:538-578`, `lib.rs:24`, `client.rs:1581-3441`, `tests/session.rs`); the JS export stays `HostedClient`.
- `src/session.rs:1313-1320`: R19.
- `src/session.rs:784-898`: close becomes close view; kill is a separate action; update the error text asserted at `tests/session.rs:1654-1701`.
- Regenerate the committed bundle with `docs/site/scripts/build-client.sh`.
- Gate: `wasm-pack test --node`, `wasm-pack build --target web`, `scripts/ci/web-browser.py`.

### L11 Docs, spec, ADRs

Scope: `docs/adr/**`, `docs/spec/**` (except the sections phase A changes), `docs/CONCEPTS.md`, `docs/SETUP.md`, `docs/consumers/**`, `docs/architecture/**`.

- Four ADRs (PLAN stage 0), numbered from the next free number.
- `docs/spec/L2.md` (all of §1-§3), `L3.md` §3-§3.4 (translation layer), `L1.md` §5.2.2, §5.3, §5.7 (new), §7.3, §9.1, `proto.md` §6.2 and §11.4, `README.md` two-minute model, `CHANGELOG.md` (`0.9.0-draft.58`, matched to `PROTOCOL_VERSION`).
- `docs/CONCEPTS.md` rewritten around the model; `docs/architecture/module-structure.md` per crate; `docs/consumers/tui.md` and `agents.md` §7 (the MCP parity tests read it).
- Gate: `just docs-check`.

## 06. Phase E: integration

One writer merges lanes onto `feat/domain-model` in phase order, then:

1. `just ci-full` (ci, e2e, agents-fleet-smoke).
2. Client gates outside ci-full: `just cockpit-test`, `just desktop-check`, desktop native tests, web `wasm-pack` and browser checks.
3. Generated surfaces, regenerated and committed together: `just docs-gen`, MCP goldens, protocol insta goldens, TUI insta snapshots, Cockpit fixtures, `index.d.ts`, the web bundle.
4. The behavior matrix from PLAN stages 2-4 on a real server: close versus kill per client; shell exit; named versus quick sessions; crash and restart; upgrade from the last release with live sessions; an old released client against the new server.
5. Migration on a copy of a real dev state directory: open into Inbox with every group accounted for; move, bulk move, and undo without changing activation IDs.

Landing: rebase onto `main`, push, open one PR from `feat/domain-model`, watch CI through green, merge with `gh pr merge --rebase --delete-branch`, then sync local `main`.

## 07. Riskiest spots

| Risk | Where | Guard |
|---|---|---|
| Upgrade handover loses live sessions | `state/upgrade_blob.rs:530-800`, `upgrade/blob.rs:27` | Keep the v1 reader; an upgrade-from-last-release e2e with live panes in phase E. |
| Exit watcher reordering breaks journal and self-exit guarantees | `runtime/client.rs:1196-1279`, `bindings.rs:170-196` | `pane_closed` stays journaled in the reaping lock; existing journal-ordering tests stay green unchanged. |
| Shared store across tests | `server_guard.rs:207-230` | R11, landed first. |
| Scope verbs silently widen or narrow authority | `kinds.rs:55-111`, `scope.rs:45-187` | R07; a test that every legacy command keeps its exact verb requirement; a test decoding a v1 grant. |
| Unknown result tag breaks old clients | `command_codec.rs:764-780` | R08; an old-client e2e. |
| Legacy snapshot drift | `info.rs:863-883`, `state/snapshot.rs:70` | R06; byte-identical goldens; stable synthetic window IDs across calls and upgrade. |
| Kill workflow races | `phux-client/src/kill.rs`, MCP `phux_kill` | Fixed target set; `NotEmpty` race test; per-target results everywhere. |
| C ABI drift | `client.h`, Cockpit Zig, fixtures | R18; ABI asserts in `cockpit-test`; regenerate fixtures in L5. |
| TUI detaches while work runs, or never detaches | `handler.rs:1481-1540` | R15; tests for close-view emptying the arrangement and for ephemeral deletion. |
| Generated-surface gates fail together | `help_inventory.rs`, refdocs, MCP parity, `kinds_table` | Regenerate all in phase E step 3 in one commit; R20. |
| Reconcile mislabels a process | `reconcile.rs` | R12; pure decision tests for pid gone, pid reused, other boot, and live match. |
| Installed service wrappers call removed verbs | `service.rs:344-400` | Hidden alias kept; `service reconcile` rewrites wrappers. |
