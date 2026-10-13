---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Migration: from Session/Window/Pane to the domain model

**TL;DR.** Today's tree already behaves more like the new model than its types admit: the server keeps one vestigial Window per session, real tabs live in client layout metadata, and desktop already treats close as release. The migration deletes Window, makes lifecycle typed instead of intercepted, renames four colliding "workspace" concepts, and gives every kill verb an explicit terminate meaning. Order and gates are in [PLAN.md](PLAN.md).

## 01. Terminology map

| Today | Becomes | Evidence |
|---|---|---|
| Core `Session{windows, active, keep_empty}` | `Session{members, lifetime, arrangement}` | `crates/phux-core/src/session.rs:57-71` |
| Core `Window{slots, layout, active}` | Deleted. Slots become members; layout becomes the session arrangement. | `crates/phux-core/src/window.rs:21-32`; only `windows.first()` is ever used, `crates/phux-server/src/state/session_table.rs:172-179` |
| Pane (a Terminal `ResourceId` in `Window.slots`) | A Terminal resource; on screen, a view | `window.rs:21-32` |
| `ResourceDescriptor.window` | `Resource.session` | `crates/phux-core/src/resource.rs:147-161` |
| `keep_empty` | `SessionLifetime` | ADR-0105 |
| `AgentSession` kind | Agent conversation (wire name kept) | ADR-0103 |
| Client `Workspace{windows: Vec<WindowState>}` | Session arrangement, client edit model | `crates/phux-client-core/src/layout/mod.rs:161-229` |
| `phux.tui.layout/v1/<session-id>` metadata | Typed arrangement record | `crates/phux-client-core/src/layout/projection.rs:12-21` |
| Session lifecycle via intercepted metadata keys | Typed organization commands | `crates/phux-server/src/runtime/client.rs:4848-4859` |
| `SessionKernel` | Resource synchronization kernel (responsibility unchanged) | ADR-0100 |
| phux-web `Session` (one attach connection) | `Connection` | `clients/phux-web/src/session.rs:537-538` |
| Work coordinator `WorkSession` | Unchanged; never aliases a Session | ADR-0092 |

## 02. Freeing the word "workspace"

Six things are called workspace today. The domain Workspace arrives in stage 6; the renames land before it.

| Today | Renamed to |
|---|---|
| `phux workspace save` and `restore` (JSON archive) | `phux archive export` and `import` |
| `phux workspace inspect` (git worktrees) | `phux project inspect` (beside the existing `project` verb) |
| `phux server --autosave` "workspace archive" | Retired by the org store; the flag keeps working as an export schedule for one release |
| `phux-client-core::layout::Workspace` | `ArrangementDoc` |
| FFI `SharedWorkspace`, `PhuxWorkspace*` C types | `SharedArrangement`, `PhuxArrangement*` |
| Plugin manifest `workspace` profiles and `phux_plugin_workspace` | `profile` and `phux_plugin_profile` |
| Desktop `src/workspace/`, Cockpit per-OS-window `Workspace` | Client-internal; renamed when touched, not blocking |

Old CLI spellings stay as hidden aliases for one release and print the new name.

## 03. TUI and tmux verbs

Rule: a verb named **kill** terminates execution, explicitly, as it always has. A verb named **close** never does.

| Binding today (`crates/phux-config/src/default.toml:143-170`) | Today | After |
|---|---|---|
| `x` kill-pane | `KILL_RESOURCE` | Unchanged: terminate that resource. Its view goes away when the resource is removed. |
| `X` kill-window | Kills every leaf of the tab | Terminate every resource in the group, after a confirm naming the count. A resource that also appears in another group or another session's arrangement is listed in the confirm. |
| `c` new-window | Spawn and add a window | Create a Terminal resource in the session and add a group to the arrangement. |
| `"` `%` `|` `-` split | Spawn and split | Create a resource and split the current group. |
| (new) `close-view`, unbound by default, in the context menu | None | Remove the view from this arrangement; the resource keeps running and stays listed in the session. |
| Context menu "close" (`crates/phux-tui/src/attach/context_menu.rs:49,70`) | Kill | Split into "Close view" and "Kill" |

Shell `exit` in a pane: the resource's exit policy is `Remove`, so it disappears, as in tmux. A session started bare (`phux`) is ephemeral, so exiting its last shell ends the session, as in tmux. A named session stays, empty, until deleted.

## 04. CLI

| Verb | After |
|---|---|
| `phux` (bare) | Ephemeral auto-named session plus a shell, attach. Today's behavior. |
| `phux new NAME` | Persistent session plus a shell. `--empty` creates no shell, replacing keep-empty. `--ephemeral` overrides. |
| `phux ls` | Sessions with lifetime, member count, and state counts (running, completed, lost); empty sessions included. |
| `phux kill TARGET` | Session: the terminate, remove, delete workflow (CONTRACTS §04). Resource: terminate. `--server` unchanged. |
| `phux rm TARGET` | New. Remove an inactive resource, or delete an empty session. |
| `phux relaunch TARGET` | New. New activation of a completed resource. |
| `phux restore [SESSION]` | New. Relaunch every resource ended by a server restart, in their sessions, with their cwd and agent resume. Replaces `workspace restore`. |
| `phux rename` | Sessions and resources. |
| `phux move-pane` | Alias of `phux mv`, a membership move; process survives (it already does). |
| `insert-pane`, `swap-pane` | Arrangement edits, unchanged in effect. |

Selectors (`crates/phux-client/src/selector.rs:7-57`) keep their grammar. `name:N` and `name:N.M` resolve against the session's shared arrangement: N is the group, M the leaf. New: a resource key prefix, `^ab12cd`, for addressing a record that has no live handle. `@N` stays the live-handle form.

## 05. MCP and agent tools

| Tool (`crates/phux-mcp/src/tool_table.rs`) | After |
|---|---|
| `phux_ls` | Returns keys, lifetime, exec state, and the caller's grant. |
| `phux_kill` | Same `confirm=true` gate; session targets run the workflow; returns per-target results. |
| `phux_new` | Takes `lifetime`. |
| `phux_spawn`, `phux_launch` | Unchanged inputs; return the `ResourceKey` and live handle. |
| `phux_move_pane` | Membership move. |
| New `phux_relaunch`, `phux_rm` | As the CLI. |
| `phux_workspace` | Becomes `phux_archive` (the archive tool), then stage 6 adds a real `phux_workspace`. |

Agents call these with their own delegated credential (CONTRACTS §03), so their actions are attributed and scoped.

## 06. Clients

| Client | Change |
|---|---|
| FFI | Project everything: keys, revisions, lifetime, members, arrangement, exec state, agent conversations, dims. Today it drops `keep_empty`, windows, agent sessions, and dims (`crates/phux-client-ffi/src/projection/topology.rs:14-89`). |
| Desktop | Already right: close is release (`clients/desktop/src/workspace/controller.ts:145-186`). Replace the "Close Session" CLI shell-out (`session-cli.ts:11-13`) with the runtime command and show the workflow's per-target results. |
| Cockpit | Close Pane and Close Tab "end work" (ADR-0114). Keep that as explicit terminate, relabel the menu items "Kill", and add "Close view". Peer-coordinator tabs already close layout-only. |
| Web | Rename its `Session` to `Connection`. Stop auto-creating `default`; open the most recent session or offer create. Close pane becomes close view; keep kill as an explicit action. |
| Mobile | Host groups become a host filter; saved terminal collections become saved filters. Neither is called a workspace. Separate repos, migrated after FFI lands. |

## 07. Saved state

- First start of a stage-2 server: build the org store from the live registry. Each session gets a key and `lifetime = keep_empty ? Persistent : Ephemeral`. Each live Terminal becomes a resource bound to its current process as activation 1, with `ProcIdentity` recorded. The session's `phux.tui.layout/v1` document becomes its arrangement.
- Existing JSON archives (schema 2, `crates/phux/src/commands/workspace/archive/model.rs:5-30`) import as new records with provenance; nothing is launched until `phux restore`.
- The old store is not deleted. Downgrade: stop the server, run the old binary; it ignores `org/`.
- Conversion is idempotent and keyed by a migration receipt in the store.

## 08. Compatibility rules

- No wire tag changes meaning. Old clients keep L1 terminal I/O, attach, input, and their metadata-based session operations through the server's translation layer (CONTRACTS §09).
- A legacy destructive operation keeps its destructive meaning; it is never quietly downgraded to a safer one.
- Translation is attributed to the calling client like any other command.
- The translation layer is deleted once every first-party client and the last two released versions speak `Organization`.

## 09. Deleted at the end

- `crates/phux-core/src/window.rs` and every `WindowId`, `active`, and slot.
- The reap cascade's window and keep-empty steps (`crates/phux-server/src/state/reap.rs`).
- Respawn-in-place of the last shell (`crates/phux-server/src/state/sessions.rs:98-110`).
- Server exit on zero sessions (`crates/phux-server/src/state/bindings.rs:192-196`).
- The intercepted session metadata keys and `CloseTabResources` (`command.rs:426-429`).
- The `--autosave` archive writer, after its release of overlap.
- Name-based joins anywhere a key exists.
