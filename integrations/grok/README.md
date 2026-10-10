# phux for Grok

This first-party Grok plugin exposes the bundled `phux mcp` server and
publishes lifecycle metadata for Grok Build sessions running inside phux
panes. The same `grok` binary is the Grok bot the server detector already
names. It requires `phux` on `PATH` and a running local phux server.

```sh
grok plugin marketplace add no-phux/phux
grok plugin install phux@phux --trust
```

From a checkout:

```sh
grok plugin install ./integrations/grok --trust
```

The MCP server starts with the plugin. Lifecycle hooks declare Grok's
identity once at session start, emit attention asks, and clear the
declaration at session end. On a server that serves agent session resources
they also open a session under Grok's pane. Typed records never carry prompt
text, tool input, or tool output. `PHUX_AGENT_TRANSCRIPT=0` turns transcript
entries off. The plugin never declares a state: the server derives it from
the stream, or from its own detector.

Hooks no-op unless `PHUX_TERMINAL_ID` is set, so Grok outside a phux pane
stays quiet.
