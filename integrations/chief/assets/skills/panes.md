---
name: panes
description: Getting work done through phux panes, shells and agents.
---
- Inventory: `phux_panes` (exact `@N` selectors, owners, cwd, agent state). `phux_list` for sessions.
- Your workbench is session `{{session}}`. Create a shell there with `phux_create` (new session) or `phux_spawn` beside an existing pane; it becomes your selected target.
- One-shot command with an exit code: `phux_run` on a shell pane. Long-running process: `phux_send_keys`/`phux_paste` then `phux_wait` for a regex or idle, and `phux_snapshot` to read it.
- Another agent pane (claude, codex, opencode, pi): `phux_agent_prompt` with a self-contained brief, then `phux_agent_wait` until `done` or `blocked`, then `phux_snapshot` to read its answer. Do not interrupt an agent that is working on someone else's task.
- Remote hosts: selectors look like `host/@N`; the same verbs work across the mesh.
- Bound your observations: prefer `tail` and targeted regexes over whole scrollbacks.
- Clean up: kill panes you spawned once their output is captured (run `phux kill --yes @N` from your shell pane).
