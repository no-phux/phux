# usage

Publishes this host's AI plan limits as the `phux.usage/v1` Global metadata
key, so every client (the phone, the desktop, a script) can show Claude Code,
Codex, and Grok Build utilization and reset times without its own
credentials.

- `publish-on-attach` and `publish-on-agent-state` run
  `phux agent usage --publish` on the server's `client-attached` and
  `agent-state-changed` hooks. A run that finds another one in progress, or
  that comes within ten seconds of the last, exits at once. Vendor requests
  are spaced two minutes apart and back off on rate limits; the key is
  rewritten only when a number or status changes, or every five minutes.
- `refresh` (palette: `plugin: Refresh plan usage`) asks every vendor now.
- `show` prints the current readings without publishing.

## What it reads, sends, and shows

It reads Claude Code's Keychain item or `~/.claude/.credentials.json`,
`~/.codex` (session ledgers and `auth.json`), and `~/.grok/auth.json`. Each
token goes only to the vendor that issued it, over HTTPS; none is written,
refreshed, or published. The published key holds plan names, percentages,
reset times, and a 12-hex account fingerprint, never an email or token.
Every client allowed `observe@global` on this server can read it: the owner
socket and every paired device. The full table is in
[plan usage](../../../docs/agents.md#plan-usage).

## Turning it off

If you added this plugin to your own `config.toml`:

```sh
phux plugin disable com.phux.usage
```

If it comes from the starter distro (`extends = [".../starter.toml"]`),
that command refuses, because the entry lives in the distro's layer, not
your file. Choose your plugins yourself instead, in your `config.toml`:

```toml
plugins = [
  { manifest = "/path/to/phux/examples/plugins/continuum/phux-plugin.toml" },
  { manifest = "/path/to/phux/examples/plugins/agent-tools/phux-plugin.toml" },
]
```

A plain `plugins` replaces the inherited list. The server reads plugin hooks
at start, so the change takes effect at its next start or upgrade.

Set `PHUX_BIN` to run a phux other than the one on `PATH`.
