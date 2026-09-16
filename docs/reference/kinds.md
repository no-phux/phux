---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-09-15
---

# phux kind catalog reference

**TL;DR.** The compiled kind catalog: server-level, substrate, and per-kind methods, the events each kind emits, and the workload-auth verb classification of every client frame and command. Rendered from `phux_protocol::kinds`, the table the classifier reads, so the page cannot drift from dispatch.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Everything below is compiled into the binary from `phux_protocol::kinds`. The same rows drive `phux --capabilities --json` and the workload-auth classifier (`docs/spec/workload-auth.md` §6), so the verbs listed for a method are the verbs dispatch requires. A method is invoked through its typed frame or command. The catalog describes methods and is never an invocation handle, and reading it grants nothing.

Verbs are the closed set of workload-auth §5. A method whose rows need only `INVENTORY` or `OBSERVE` is read-only; any other verb can change server state; a method no row admits by verb (the `COMMAND` envelope, a denied method) counts as mutating. A gate is the `HELLO_OK` feature bit a client must see before relying on the method, named as `phux status --json` names it under `features`. In `phux --capabilities --json` a gate is `{ "feature": name, "mask": value }`, where `mask` is the bit's value in `HELLO_OK.server_caps.features`, not a bit index.

## Server methods

Addressed to the server or the connection rather than to one resource.

| Method | Carrier | Requires | Mutating | Gate | Status |
|---|---|---|---|---|---|
| `HELLO` | frame `0x01` | exempt: handshake | no | none | shipped |
| `PING` | frame `0x7f` | exempt: liveness | no | none | shipped |
| `ATTACH` | frame `0x02` | OBSERVE, CREATE, BIND | yes | none | shipped |
| `DETACH` | frame `0x03` | exempt: cleanup | no | none | shipped |
| `VIEWPORT_RESIZE` | frame `0x20` | BIND | yes | none | shipped |
| `COMMAND` | frame `0x31` | nested | yes | none | shipped |
| `LIST_DIRECTORY` | frame `0x55` | INVENTORY | no | `list_directory` | shipped |
| `UPGRADE` | command `0x0e` | SIGNAL | yes | none | shipped |
| `SHUTDOWN` | command `0x16` | SIGNAL (owner socket only) | yes | `shutdown` | shipped |
| `OPEN_LISTENER` | command `0x1c` | SIGNAL (owner socket only) | yes | `open_listener` | shipped |
| `DETACH_CLIENTS` | command `0x13` | SIGNAL | yes | none | shipped |
| `GET_PERF` | command `0x18` | OBSERVE, BIND | yes | `get_perf` | shipped |
| `phux.session.create/v1` | metadata key | CREATE, BIND | yes | none | shipped |
| `phux.session.name/v1` | metadata key | BIND | yes | none | shipped |
| `phux.session.keep_empty/v1` | metadata key | CREATE, BIND, SIGNAL | yes | `keep_empty_sessions` | shipped |
| `phux.config.reload/v1` | metadata key | SIGNAL | yes | none | shipped |
| `phux.approval.decide/v1/` | metadata key | SIGNAL | yes | `approvals` | shipped |
| `phux.whoami/v1` | metadata key | exempt: self | no | `whoami` | shipped |

## Server events

| Event | Tag |
|---|---|
| `journal_gap` | `0x0b` |
| `approval_requested` | `0x0d` |
| `approval_decided` | `0x0e` |

## Substrate methods

Answered by every resource kind.

| Method | Carrier | Requires | Mutating | Gate | Status |
|---|---|---|---|---|---|
| `SPAWN_RESOURCE` | frame `0x22` | CREATE, BIND | yes | none | shipped |
| `ATTACH_RESOURCE` | command `0x01` | OBSERVE, BIND | yes | none | shipped |
| `DETACH_RESOURCE` | command `0x02` | exempt: cleanup | no | none | shipped |
| `KILL_RESOURCE` | command `0x03` | SIGNAL | yes | none | shipped |
| `KILL_RESOURCE_IF` | command `0x1b` | SIGNAL | yes | `conditional_kill` | shipped |
| `KILL_RESOURCES` | command `0x09` | SIGNAL | yes | none | shipped |
| `CLOSE_TAB_RESOURCES` | command `0x1d` | SIGNAL | yes | `close_tab_resources` | shipped |
| `SUBSCRIBE_RESOURCE_EVENTS` | command `0x0d` | OBSERVE | no | none | shipped |
| `SUBSCRIBE_EVENTS` | frame `0x41` | OBSERVE | no | none | shipped |
| `GET_STATE` | command `0x05` | INVENTORY | no | none | shipped |
| `GET_METADATA` | frame `0x50` | OBSERVE | no | none | shipped |
| `SET_METADATA` | frame `0x51` | CREATE, BIND, SIGNAL | yes | none | shipped |
| `DELETE_METADATA` | frame `0x52` | BIND | yes | none | shipped |
| `LIST_METADATA` | frame `0x53` | INVENTORY | no | none | shipped |
| `SUBSCRIBE_METADATA` | frame `0x54` | OBSERVE | no | none | shipped |

## Substrate events

| Event | Tag |
|---|---|
| `pane_spawned` | `0x04` |
| `pane_closed` | `0x05` |
| `source_gap` | `0x0c` |

## Kind `TERMINAL`

Wire tag `0`; gate: none.

### Methods

The kind's facet.

| Method | Carrier | Requires | Mutating | Gate | Status |
|---|---|---|---|---|---|
| `INPUT_KEY` | frame `0x10` | INPUT | yes | none | shipped |
| `INPUT_PASTE` | frame `0x11` | INPUT | yes | none | shipped |
| `INPUT_MOUSE` | frame `0x12` | INPUT | yes | none | shipped |
| `INPUT_FOCUS` | frame `0x14` | INPUT | yes | none | shipped |
| `INPUT_RAW` | frame `0x13` | INPUT | yes | none | spec-only |
| `INPUT_TERMINAL_REPLY` | frame `0x17` | INPUT | yes | `terminal_reply` | shipped |
| `RESIZE_TERMINAL` | frame `0x23` | BIND | yes | none | shipped |
| `HISTORY_REQUEST` | frame `0x16` | OBSERVE | no | none | shipped |
| `FRAME_ACK` | frame `0x21` | OBSERVE | no | none | shipped |
| `MOVE_RESOURCE` | frame `0x2a` | BIND | yes | `move_resource` | shipped |
| `GET_SCREEN` | command `0x07` | OBSERVE | no | none | shipped |
| `ROUTE_INPUT` | command `0x08` | INPUT | yes | none | shipped |
| `APPLY_INPUT` | command `0x14` | INPUT | yes | `acknowledged_input` | shipped |
| `PUT_FILE` | command `0x15` | INPUT | yes | `file_upload` | shipped |
| `TRANSCRIBE` | command `0x19` | INPUT | yes | `transcribe` | shipped |
| `GET_TERMINAL_STATE` | command `0x0c` | INVENTORY | no | none | shipped |
| `ACQUIRE_INPUT` | command `0x0f` | BIND | yes | none | shipped |
| `RELEASE_INPUT` | command `0x10` | BIND | yes | none | shipped |
| `SIGNAL_TERMINAL` | command `0x11` | SIGNAL | yes | none | shipped |
| `REPORT_ASKED` | command `0x12` | BIND | yes | none | shipped |
| `REPORT_AGENT_STATE` | command `0x17` | BIND | yes | `report_agent_state` | shipped |

### Events

| Event | Tag |
|---|---|
| `command_started` | `0x00` |
| `command_finished` | `0x01` |
| `title_changed` | `0x02` |
| `bell` | `0x03` |
| `dirty` | `0x06` |
| `idle` | `0x07` |
| `terminal_control` | `0x08` |
| `asked` | `0x09` |
| `cwd_changed` | `0x0a` |

### Resource metadata keys

- `phux.agent/v1`
- `phux.agent-session/v1`
- `phux.pane-occupant/v1`
- `phux.tags/v1`
- `phux.link/v1`

## Kind `AGENT_SESSION`

Wire tag `1`; gate: `resource_kinds`.

### Methods

The kind's facet.

| Method | Carrier | Requires | Mutating | Gate | Status |
|---|---|---|---|---|---|
| `APPEND_RESOURCE_OUTPUT` | command `0x1a` | BIND, INPUT | yes | `resource_kinds` | shipped |

### Events

None.

### Resource metadata keys

None.

## Client frame classification

Every client-originated frame lands on exactly one row.

| Case | Requires | Subject |
|---|---|---|
| `HELLO` | exempt: handshake | `None` |
| `PING` | exempt: liveness | `None` |
| `DETACH` | exempt: cleanup | `CallingConnection` |
| `ATTACH` existing/last target | OBSERVE+BIND | `ResolvedGroup` |
| `ATTACH` create-if-missing | OBSERVE+CREATE+BIND | `SelectedLocalGroup` |
| `HISTORY_REQUEST` | OBSERVE | `NamedTerminal` |
| `FRAME_ACK` | OBSERVE | `NamedTerminal` |
| `COMMAND` | nested | `None` |
| `SUBSCRIBE` (unallocated) | deny | `None` |
| `INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY` | INPUT | `NamedTerminal` |
| `VIEWPORT_RESIZE` | BIND | `AttachedTerminals` |
| `SPAWN_RESOURCE { satellite: Some, owner_terminal: None }` | CREATE | `SatelliteHost` |
| `SPAWN_RESOURCE { satellite: None, owner_terminal: None }` | CREATE | `PayloadGroup` |
| `SPAWN_RESOURCE { satellite: None, owner_terminal: Some }` | CREATE+BIND | `OwnerTerminalGroup` |
| `SPAWN_RESOURCE { satellite: Some, owner_terminal: Some }` | deny | `None` |
| `SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: None, parent: Some }` | CREATE+BIND | `ParentTerminalGroup` |
| `SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: Some(H), parent: Some(Satellite { H, .. }) }` | CREATE+BIND | `SatelliteHostAndParent` |
| `SPAWN_RESOURCE { kind: not TERMINAL, parent: None }` or `{ kind: TERMINAL, parent: Some }` | deny | `None` |
| `RESIZE_TERMINAL` | BIND | `NamedTerminal` |
| `MOVE_RESOURCE` | BIND | `MovedAndOwnerTerminals` |
| `SUBSCRIBE_EVENTS { terminal: Some }` | OBSERVE | `NamedTerminal` |
| `SUBSCRIBE_EVENTS { terminal: None }` | OBSERVE | `ObservableTerminals` |
| `GET_METADATA { Global, "phux.whoami/v1" }` | exempt: self | `CallingConnection` |
| Other `GET_METADATA` | OBSERVE | `MetadataScope` |
| `SET_METADATA { Global, "phux.session.create/v1" }` | CREATE+BIND | `Global { owner_uds_only: false }` |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true` | CREATE+BIND | `Global { owner_uds_only: false }` |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0false` | SIGNAL | `NamedSession` |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value | deny | `None` |
| `SET_METADATA { Global, "phux.config.reload/v1" }` | SIGNAL | `Global { owner_uds_only: false }` |
| `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value `approve` or `deny` | SIGNAL | `HeldAction` |
| `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with a malformed id or any other value | deny | `None` |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results | deny | `None` |
| `SUBSCRIBE_METADATA` targeting that result namespace | deny | `None` |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1`, `phux.whoami/v1`, or a `phux.approval/v1/<id>` record, or `DELETE_METADATA` targeting `phux.config.reload/v1`, `phux.session.keep_empty/v1`, or a `phux.approval.decide/v1/<id>` key | deny | `None` |
| Other `SET_METADATA`, `DELETE_METADATA` | BIND | `MetadataScope` |
| `LIST_METADATA` | INVENTORY | `MetadataScope` |
| `LIST_DIRECTORY` | INVENTORY | `Global { owner_uds_only: false }` |
| Other `SUBSCRIBE_METADATA` | OBSERVE | `MetadataScope` |
| Unknown, wrong-direction, retired, or otherwise unclassified frame | deny | `None` |

## Command classification

Every command nested in `COMMAND` lands on exactly one row; the envelope alone grants nothing.

| Case | Requires | Subject |
|---|---|---|
| `SPAWN` (unallocated) | deny | `None` |
| `ATTACH_RESOURCE` | OBSERVE+BIND | `NamedTerminal` |
| `DETACH_RESOURCE` | exempt: cleanup | `CallingConnection` |
| `KILL_RESOURCE` | SIGNAL | `NamedTerminal` |
| `KILL_RESOURCE_IF` | SIGNAL | `NamedTerminal` |
| `GET_SCREEN` | OBSERVE | `NamedTerminal` |
| `ROUTE_INPUT`, `APPLY_INPUT` | INPUT | `NamedTerminal` |
| `KILL_RESOURCES` | SIGNAL | `EveryNamedTerminal` |
| `CLOSE_TAB_RESOURCES` | SIGNAL | `EveryNamedTerminal` |
| `RESIZE_TERMINAL` (unallocated) | deny | `None` |
| `GET_STATE { SERVER }` | INVENTORY | `InventoryMatches` |
| `RUN_HOOK` (unallocated) | deny | `None` |
| `GET_TERMINAL_STATE` | INVENTORY | `NamedTerminal` |
| `SUBSCRIBE_RESOURCE_EVENTS` | OBSERVE | `NamedTerminal` |
| `UPGRADE` | SIGNAL | `Global { owner_uds_only: false }` |
| `ACQUIRE_INPUT`, `RELEASE_INPUT` | BIND | `NamedTerminal` |
| `SIGNAL_TERMINAL` | SIGNAL | `NamedTerminal` |
| `REPORT_ASKED`, `REPORT_AGENT_STATE` | BIND | `NamedTerminal` |
| `PUT_FILE` | INPUT | `NamedTerminal` |
| `TRANSCRIBE` | INPUT | `NamedTerminal` |
| `DETACH_CLIENTS { session: Some }` | SIGNAL | `ResolvedGroup` |
| `DETACH_CLIENTS { session: None }` | SIGNAL | `Global { owner_uds_only: false }` |
| `SHUTDOWN` | SIGNAL + owner-UDS transport | `Global { owner_uds_only: true }` |
| `OPEN_LISTENER` | SIGNAL + owner-UDS transport | `Global { owner_uds_only: true }` |
| `GET_PERF { reset: false }` | OBSERVE | `Global { owner_uds_only: false }` |
| `GET_PERF { reset: true }` | OBSERVE+BIND | `Global { owner_uds_only: false }` |
| `APPEND_RESOURCE_OUTPUT` | BIND+INPUT | `ParentOfNamed` |
| Unknown, retired, or otherwise unclassified command tag | deny | `None` |
