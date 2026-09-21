---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-09-15
---

# phux CLI/MCP parity reference

**TL;DR.** Every MCP tool the adapter serves, mapped to the `phux` verb it mirrors (or why it has none), whether it runs in-process or through the CLI residue and why, and its read-only and destructive annotations. Rendered from the tool table the parity gate checks, so it cannot drift from the adapter.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every tool the MCP adapter (`phux mcp`) serves, the `phux` verb it mirrors, and how it runs. `in-process` tools spawn no subprocess and return the document the CLI verb prints, built by the same `phux-client` function. The CLI residue runs the canonical `phux` binary with argv, never a shell, for the reason listed. The parity gate holds this table to the live tool catalog, the CLI grammar, and the kind table, so it cannot drift from the adapter.

| Tool | CLI verb | Runs | Read-only | Destructive |
|---|---|---|---|---|
| `phux_ls` | `phux ls` | in-process | yes | no |
| `phux_snapshot` | `phux snapshot` | in-process | yes | no |
| `phux_send_keys` | `phux send-keys` | in-process | no | yes |
| `phux_paste` | `phux paste` | in-process | no | yes |
| `phux_run` | `phux run` | CLI | no | yes |
| `phux_wait` | `phux wait` | in-process | yes | no |
| `phux_new` | `phux new` | CLI | no | no |
| `phux_kill` | `phux kill` | in-process | no | yes |
| `phux_detach` | `phux detach` | in-process | no | yes |
| `phux_watch` | `phux watch` | in-process | yes | no |
| `phux_ask` | `phux ask` | in-process | no | no |
| `phux_launch` | `phux launch` | CLI | no | yes |
| `phux_spawn` | `phux spawn` | in-process | no | yes |
| `phux_signal` | `phux signal` | in-process | no | yes |
| `phux_tag` | `phux tag` | in-process | no | no |
| `phux_rename` | `phux rename` | in-process | no | no |
| `phux_insert_pane` | `phux insert-pane` | in-process | no | no |
| `phux_move_pane` | `phux move-pane` | in-process | no | no |
| `phux_swap_pane` | `phux swap-pane` | in-process | no | no |
| `phux_workspace` | `phux workspace` | CLI | no | no |
| `phux_plugin_action` | `phux config run` | in-process | no | yes |
| `phux_plugin_workspace` | none (automation only) | in-process | yes | no |
| `phux_agent_list` | `phux agent list` | CLI | yes | no |
| `phux_agent_show` | `phux agent show` | CLI | yes | no |
| `phux_agent_explain` | `phux agent explain` | CLI | yes | no |
| `phux_agent_set` | `phux agent set` | in-process | no | no |
| `phux_agent_clear` | `phux agent clear` | in-process | no | no |
| `phux_agent_wait` | `phux agent wait` | CLI | yes | no |
| `phux_agent_send_keys` | `phux agent send-keys` | CLI | no | yes |
| `phux_agent_prompt` | `phux agent prompt` | CLI | no | yes |
| `phux_agent_answer` | `phux agent answer` | CLI | no | yes |
| `phux_agent_start` | `phux agent start` | CLI | no | yes |
| `phux_agent_session_open` | `phux agent session open` | CLI | no | no |
| `phux_agent_session_close` | `phux agent session close` | CLI | no | yes |
| `phux_agent_emit` | `phux agent emit` | CLI | no | yes |
| `phux_agent_log` | `phux agent log` | CLI | no | no |
| `phux_status` | `phux status` | CLI | yes | no |
| `phux_doctor` | `phux doctor` | CLI | yes | no |
| `phux_whoami` | `phux whoami` | CLI | yes | no |
| `phux_resource_show` | `phux resource show` | in-process | yes | no |
| `phux_resource_wait` | `phux resource wait` | in-process | yes | no |
| `phux_resource_methods` | `phux resource methods` | in-process | yes | no |
| `phux_approvals` | `phux approvals` | in-process | yes | no |
| `phux_approve` | `phux approve` | in-process | no | yes |

## CLI residue

These tools still run the `phux` binary as a subprocess, bounded in time and output:

- `phux_run`: the sentinel bracketing, the child's mirrored exit code, and the run deadline are the CLI command's, and a failing command must still return its RunResult.
- `phux_new`: `phux new` auto-starts a local server when none is listening, which runs the phux binary; `phux_client::session` has the create, but not that server lifecycle.
- `phux_launch`: integration resolution (enabled plugins, manifests, argv templates, native-session restore) lives in the CLI command.
- `phux_workspace`: git worktree inspection and the session archive save/restore live in the CLI command.
- `phux_agent_list`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_show`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_explain`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_wait`: the result document's `detection` field is the detector projection, which is built in the phux binary, not phux-client.
- `phux_agent_send_keys`: key-spec validation, the occupant check, operation-id minting, and the ADR-0076 error-code table live in the CLI command.
- `phux_agent_prompt`: operation-id minting, the fused wait's result document, and the ADR-0076 error-code table live in the CLI command.
- `phux_agent_answer`: live-ask correlation and suggestion validation live in the CLI command.
- `phux_agent_start`: integration resolution and the detector readiness wait live in the CLI command.
- `phux_agent_session_open`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_session_close`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_emit`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_log`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_status`: the status document (peer-credential pid, service state, log paths) is assembled by the CLI command.
- `phux_doctor`: the health checks are the CLI's; a second copy could disagree with `phux doctor`.
- `phux_whoami`: the `whoami` feature check and the refusal document are the CLI command's.

## Automation-only tools

- `phux_plugin_workspace`: lists the workspace profiles configured plugin manifests declare; `phux plugin list` and `phux config plugins` list the manifests, not their workspace profiles.

## CLI verbs without a tool

Agent-facing verbs in the JSON index of `docs/consumers/agents.md` that have no MCP tool yet:

- `phux config agents`: local config inventory of declared agent integrations; `phux_agent_list` covers the live agents.
- `phux host ls`: operator inventory of the host registry, not an agent action.
- `phux pair`: mints a pairing secret; credential handling stays outside the model-facing set.
- `phux mcp`: launches this adapter itself.
- `phux deny`: `phux_approve` decides both ways: `decision: deny` is `phux deny` (ADR-0128).
- `phux resize`: not exposed over MCP yet; a known parity gap.
- `phux rec`: not exposed over MCP yet; a recording is written to a file on the adapter's host.
- `phux play`: not exposed over MCP yet; playback replays a file from the adapter's host.

## Annotation sources

`Read-only` is the kind table's `mutating` rule: a tool is read-only only when no method it can send changes server state. `Destructive` comes from the kind table's `MethodSpec.dangerous` flag (ADR-0128), or a method that needs `INPUT`.
