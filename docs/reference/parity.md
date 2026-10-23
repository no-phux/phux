# CLI and MCP parity

Each MCP tool, the `phux` command it mirrors, and its read-only and destructive hints.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every tool the MCP adapter (`phux mcp`) serves, the `phux` verb it mirrors, and how it runs. `in-process` tools spawn no subprocess and return the document the CLI verb prints, built by the same `phux-client` function. The CLI residue runs the canonical `phux` binary with argv, never a shell, for the reason listed. The parity gate holds this table to the live tool catalog, the CLI grammar, and the kind table, so it cannot drift from the adapter.

| Tool | CLI verb | Runs | Read-only | Destructive |
|---|---|---|---|---|
| `phux_operation_status` | none (automation only) | in-process | yes | no |
| `phux_ls` | `phux ls` | in-process | yes | no |
| `phux_upload` | `phux upload` | in-process | no | yes |
| `phux_snapshot` | `phux snapshot` | in-process | yes | no |
| `phux_send_keys` | `phux send-keys` | in-process | no | yes |
| `phux_paste` | `phux paste` | in-process | no | yes |
| `phux_run` | `phux run` | CLI | no | yes |
| `phux_wait` | `phux wait` | in-process | yes | no |
| `phux_new` | `phux new` | CLI | no | no |
| `phux_kill` | `phux kill` | in-process | no | yes |
| `phux_rm` | `phux rm` | in-process | no | yes |
| `phux_relaunch` | `phux relaunch` | in-process | no | no |
| `phux_organize` | `phux organize` | in-process | no | no |
| `phux_detach` | `phux detach` | in-process | no | yes |
| `phux_watch` | `phux watch` | in-process | yes | no |
| `phux_ask` | `phux ask` | in-process | no | no |
| `phux_launch` | `phux launch` | CLI | no | yes |
| `phux_spawn` | `phux spawn` | in-process | no | yes |
| `phux_signal` | `phux signal` | in-process | no | yes |
| `phux_tag` | `phux tag` | in-process | no | no |
| `phux_rename` | `phux rename` | in-process | no | no |
| `phux_insert_pane` | `phux insert-pane` | in-process | no | no |
| `phux_move_pane` | `phux mv` | in-process | no | no |
| `phux_swap_pane` | `phux swap-pane` | in-process | no | no |
| `phux_archive` | `phux archive` | CLI | no | no |
| `phux_project_inspect` | `phux project inspect` | CLI | yes | no |
| `phux_plugin_action` | `phux config run` | in-process | no | yes |
| `phux_plugin_profile` | none (automation only) | in-process | yes | no |
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
| `phux_mail_send` | `phux mail send` | in-process | no | no |
| `phux_mail_read` | `phux mail read` | in-process | no | no |
| `phux_mail_ack` | `phux mail ack` | in-process | no | no |
| `phux_claim_take` | `phux claim take` | in-process | no | no |
| `phux_claim_renew` | `phux claim renew` | in-process | no | no |
| `phux_claim_release` | `phux claim release` | in-process | no | no |
| `phux_claim_list` | `phux claim list` | in-process | yes | no |
| `phux_status` | `phux status` | CLI | yes | no |
| `phux_doctor` | `phux doctor` | CLI | yes | no |
| `phux_whoami` | `phux whoami` | CLI | yes | no |
| `phux_perf` | `phux perf` | CLI | yes | no |
| `phux_perf_trace` | `phux perf trace` | CLI | no | no |
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
- `phux_archive`: the archive capture, its agent-session bridge, and the import live in the CLI command.
- `phux_project_inspect`: git worktree inspection runs `git` from the CLI command; phux-client has no git.
- `phux_agent_list`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_show`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_explain`: the detector projection (manifest replay over the pane's screen, plus `[[plugins]]` agent declarations) is built in the phux binary, not phux-client.
- `phux_agent_wait`: the result document's `detection` field is the detector projection, which is built in the phux binary, not phux-client.
- `phux_agent_send_keys`: key-spec validation, the occupant check, operation-id minting, and the error-code table live in the CLI command.
- `phux_agent_prompt`: operation-id minting, the fused wait's result document, and the error-code table live in the CLI command.
- `phux_agent_answer`: live-ask correlation and suggestion validation live in the CLI command.
- `phux_agent_start`: integration resolution and the detector readiness wait live in the CLI command.
- `phux_agent_session_open`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_session_close`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_emit`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_agent_log`: the Terminal-to-AgentSession resolution and the result documents are assembled in the CLI command (`commands/agent/resource_session.rs`).
- `phux_status`: the status document (peer-credential pid, service state, log paths) is assembled by the CLI command.
- `phux_doctor`: the health checks are the CLI's; a second copy could disagree with `phux doctor`.
- `phux_whoami`: the `whoami` feature check and the refusal document are the CLI command's.
- `phux_perf`: the report document and the dev-mode desktop's socket discovery are the CLI command's.
- `phux_perf_trace`: the capture window, the trace file, and the merged desktop timeline are the CLI command's.

## Automation-only tools

- `phux_operation_status`: observe an adapter-scoped mutation without blocking on approval.
- `phux_plugin_profile`: lists the profiles configured plugin manifests declare; `phux plugin list` and `phux config plugins` list the manifests, not their profiles.

## CLI verbs without a tool

Agent-facing verbs that have no MCP tool yet:

- `phux config distro list`: local distro package inventory; no server action.
- `phux config distro current`: local configured selection; no server action.
- `phux config distro use`: operator config-file selection; reload is explicitly separate.
- `phux config distro off`: operator config-file selection; reload is explicitly separate.
- `phux logs`: local operator log inventory and tailing, not a model-facing server action.
- `phux update`: replaces the operator's installed binaries; installation management stays outside MCP.
- `phux channel`: changes the operator's installed release channel; installation management stays outside MCP.
- `phux config agents`: local config inventory of declared agent integrations; `phux_agent_list` covers the live agents.
- `phux host add`: operator host-registry enrollment, not an agent action.
- `phux host ls`: operator inventory of the host registry, not an agent action.
- `phux host discover`: operator tailnet inventory for enrollment, not an agent action.
- `phux host renew`: operator host-registry certificate renewal, not an agent action.
- `phux host upgrade`: operator host-registry transport upgrade, not an agent action.
- `phux pair`: mints a pairing secret; credential handling stays outside the model-facing set.
- `phux mcp`: launches this adapter itself.
- `phux deny`: `phux_approve` decides both ways: `decision: deny` is `phux deny`.
- `phux resize`: not exposed over MCP yet; a known parity gap.
- `phux restore`: an operator's after-restart sweep; an agent relaunches the one pane it means with `phux_relaunch`.
- `phux rec`: not exposed over MCP yet; a recording is written to a file on the adapter's host.
- `phux play`: not exposed over MCP yet; playback replays a file from the adapter's host.
- `phux rec mark`: not exposed over MCP yet; it marks recordings written on the adapter's host.

## Annotation sources

`Read-only` is the kind table's `mutating` rule: a tool is read-only only when no method it can send changes server state. `Destructive` comes from the kind table's `MethodSpec.dangerous` flag, or a method that needs `INPUT`.
