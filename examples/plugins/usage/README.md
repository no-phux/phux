# usage

Publishes this host's AI plan limits as the `phux.usage/v1` Global metadata
key, so every client (the phone, the desktop, a script) can show Claude Code,
Codex, and Grok Build utilization and reset times without its own
credentials.

- `publish-on-attach` and `publish-on-agent-state` run `phux usage --publish`
  on the server's `client-attached` and `agent-state-changed` hooks. The verb
  spaces vendor requests two minutes apart, backs off on rate limits, and
  rewrites the key only when a plan changed or every five minutes.
- `refresh` (palette: `plugin: Refresh plan usage`) asks every vendor now.
- `show` prints the current readings without publishing.

Set `PHUX_BIN` to run a phux other than the one on `PATH`. The document
shape and the sources are described in
[plan usage](../../../docs/agents.md#plan-usage).
