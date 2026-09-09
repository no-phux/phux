# phux for Claude Code

This first-party Claude Code plugin exposes the bundled `phux mcp` server and
publishes lifecycle metadata for Claude sessions running inside phux panes. It
requires `phux` and `phux-mcp` on `PATH` and a running local phux server.

```sh
claude plugin marketplace add no-phux/phux
claude plugin install phux@phux
```

The MCP server starts with the plugin and contributes the authoritative phux
tool catalog. Lifecycle hooks declare Claude's identity once at session start,
emit attention asks for permission and elicitation prompts, and clear the
declaration at session end. On a phux server that serves agent session
resources they also open a session under Claude's pane and append one record
per hook event (`session_start`, `prompt` as a character count, `tool_start`
and `tool_end` by tool name, `ask`, `notification`, `stop`, `session_end`);
prompt text, tool input, and tool output are never forwarded unless
`PHUX_AGENT_EMIT_RAW=1` opts the raw payload in. The plugin never declares a
state: the server derives it from the stream, or from its own detector.

For development:

```sh
npm ci
npm run gates
claude --plugin-dir .
```
