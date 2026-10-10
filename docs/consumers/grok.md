---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-10-10
---

# Grok integration

**TL;DR.** Install the `phux` Grok plugin for MCP tools, the `using-phux`
skill, and pane identity hooks. Grok Build and the `grok` bot use the same
binary. The server detector already recognizes that binary; the plugin
publishes the same lifecycle Claude, Pi, and OpenCode publish.

## Install

Install `phux` first and start a local server. Then:

```sh
grok plugin marketplace add no-phux/phux
grok plugin install phux@phux --trust
```

From a checkout, `grok plugin install ./integrations/grok --trust` loads the
same plugin. Hooks stay quiet unless `PHUX_TERMINAL_ID` is set, which a phux
pane exports for its child.

## First shared-terminal walkthrough

1. Start `phux` and leave a shell pane free.
2. Open another pane and run `grok` there.
3. Ask Grok to list sessions and snapshot the idle shell. The snapshot must
   be the shell, not Grok's own pane.

`phux agent show` on Grok's pane reports kind `grok` after the session
starts. A permission or idle notification raises an ask. Session end clears
the declaration.
