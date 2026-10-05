---
name: using-phux-mcp
description: Drives persistent terminals and terminal-hosted agents through the phux MCP server. Use when phux MCP tools are available for interactive programs, durable shell state, bounded observation, or agent supervision; use using-phux for direct CLI work.
compatibility: Requires an MCP host configured to launch the installed phux mcp stdio server.
metadata:
  version: "0.50.1" # x-release-please-version
---

# Using phux through MCP

Treat the live MCP catalog as the API contract. Inspect `tools/list` in an MCP
session or run `phux mcp --schema` outside one. Do not copy argument schemas or
infer fields from examples. `phux mcp` is the installed stdio entry point;
registration does not start a phux server.

Native OMP, OpenCode, and Pi integrations also register `phux_*` tools, but
their names and argument fields are not the MCP contract. Use
`using-phux-tools` for those integrations; do not mix `phux_list` with
MCP's `phux_ls`, or native `retain_seconds` with MCP's `retain_secs`.

## Workflow

1. Discover sessions with `phux_ls` and choose a returned direct selector such
   as `@7` for writes.
2. Read with `phux_snapshot` or an agent level read before acting.
3. Act with `phux_run` for one command, paste plus `phux_send_keys` for
   multiline interactive input, or `phux_agent_prompt` for an agent turn.
4. Observe with a finite timeout using `phux_wait`, `phux_watch`,
   `phux_agent_wait`, or `phux_resource_wait` for a process's exit (spawn it
   with `retain_secs` so a finished run keeps its status).
5. Re-read state. A quiet pane, successful write, or ended watcher is not proof
   of completion.
6. Diagnose unexpected state with the read-only `phux_status` and `phux_doctor`
   tools before guessing.

## Lifecycle and delivery

Agent show/list operations are level reads. Completion requires an observed transition.
Prefer `phux_agent_prompt` when submitting work because it combines the write
and edge observation without a race. An already-idle pane does not prove that a
turn completed.

Always use finite timeout values. A `delivery_unknown` result is terminal:
inspect the pane and do not resend because the first operation may still land.
Serialize acknowledged fleet prompts because the input lane is server-scoped.
A paste inserts text but does not submit it.

Cancellation or a local tool deadline only ends observation; it does not undo
input or stop the terminal process. For `phux_resource_wait`, retain the
returned cursor for resumption. `gone`, `timed_out`, or lost journal evidence
must not be reported as a successful process exit.

## Safety

- MCP input mutates a real PTY that a human may share. Layout changes never
  move the human's client-local focus.
- Prefer exact pane ids for writes; session names and tags may select sets.
- Never send input into the pane hosting your own harness. Create a sibling
  terminal for shell work; do not send shell commands into an agent TUI.
- Before kill, detach, or a destructive signal, resolve and display the target,
  inspect its state, explain the effect, obtain affirmative confirmation, pass
  `confirm: true`, then verify the inventory change.
- A call the server holds for approval waits for a human decision:
  `phux_approvals` lists it, and you cannot approve your own.
- Tool failures use `isError: true`; inspect their content. A stopped server or
  failed diagnostic check may be structured successful output.

There is no attach tool, headless focus tool, durable input lease, scheduler,
credential mutation surface, or unbounded watch call. Keep those concerns in
the orchestrator.
