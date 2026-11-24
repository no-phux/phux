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
and `tool_end` by tool name, `ask`, `notification`, `stop`, `session_end`).
Typed records never carry prompt text, tool input, or tool output. A
`provider_raw` record in the `phux.transcript/v1` convention
carries the conversation for phones and other stream readers: the prompt, each
finished or failed tool call (its name, a one-line argument summary, and its
status), and the turn's last reply. It is on by default, because the same
text is already on the pane's screen under the same access control;
`PHUX_AGENT_TRANSCRIPT=0` turns it off, and `PHUX_AGENT_TRANSCRIPT=full` adds
the tail of each tool's output. `PHUX_AGENT_EMIT_RAW=1` separately
opts the whole raw hook payload in. The plugin never declares a
state: the server derives it from the stream, or from its own detector.

Mail sent to Claude's pane reaches the conversation through a separate mail
hook (`scripts/phux-mail.sh`), so its timeout is spent on the take alone:
`phux mail hook-context` returns it as `additionalContext` on `SessionStart`,
`UserPromptSubmit`, `PostToolUse` and `PostToolUseFailure`, and as a `block`
decision on `Stop`, so Claude goes on and handles it. Each message is
delivered once. Claude idle at its prompt runs no hook, so its mail waits in
the store until the next one; nothing is ever typed into the pane. With
`PHUX_AGENT_REPORTING=0` the mail hook is silent too, since an SDK
controller owns that conversation.

With this plugin installed, `phux chat run NAME -- claude` permits mail-only
delivery without a pane: the launcher supplies a nonempty `PHUX_CHAT_RECORD`
and `PHUX_AGENT_ADDRESS`, and pins `PHUX_AGENT_PHUX_BIN` to its own binary.
The hook forwards that environment to `mail hook-context`. An address alone
does not enable delivery, and lifecycle reporting still requires a pane.
Chat mail is colleague data, not authoritative user instructions. This does
not wake an idle Claude; delivery still waits for the next registered hook.

For development:

```sh
npm ci
npm run gates
claude --plugin-dir .
```

`npm run gates` validates the plugin with the pinned `@anthropic-ai/claude-code`
dev dependency, whose install script fetches the `claude` binary. npm 12, which
CI and the release use, runs only install scripts listed under `allowScripts`,
so bump that entry together with the dev dependency
(`npm install-scripts approve @anthropic-ai/claude-code`).
