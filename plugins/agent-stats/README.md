# agent-stats

Publishes how much work each pane's agent has done as the
`phux.agent-stats/v1` Global metadata key, so every client (the phone, the
desktop, a script) can size an agent by what it has finished. It reads only
phux's own agent states: no files, no network.

- `record-agent-state` runs `phux agent stats --publish` on every
  `agent-state-changed` hook and counts the change.
- `prune-on-exit` runs it on `pane-exit` and forgets that pane.
- `publish-on-attach` runs it on `client-attached` and republishes.
- `show` (palette: `plugin: Show agent stats`) prints the document.

Each run updates a small state file under the profile's state directory
(`agent-stats/`, one file per server socket) under an exclusive lock, then
writes the key while still holding it, so concurrent hooks never lose a
count or publish an older document over a newer one.

## Turning it on

Add the manifest to your `config.toml`, from a phux source checkout:

```toml
plugins = [
  { manifest = "/path/to/phux/plugins/agent-stats/phux-plugin.toml" },
]
```

Or install it: `phux plugin install no-phux/phux/plugins/agent-stats`. The
server reads plugin hooks at start, so it takes effect at its next start or
upgrade. Set `PHUX_BIN` to run a phux other than the one on `PATH`.

## Contract

Key `phux.agent-stats/v1`, Global scope, written by the host's owner and
readable by every client allowed `observe@global`. The value is compact
JSON, at most 16 KiB:

```json
{"v":1,"agents":{"12":{"kind":"claude","name":"fox","turns":4,"working_s":731,"first_seen_ms":1762500000000,"updated_ms":1762500900000}}}
```

| Field | Meaning |
| --- | --- |
| `v` | Always `1`. Fields are only ever added; ignore unknown ones. |
| `agents` | One entry per pane, keyed by its terminal id (the `@N` number, as text). |
| `kind` | The agent kind (`claude`, `codex`, ...). |
| `name` | The agent's name, at most 48 characters; empty when anonymous. |
| `turns` | Tasks finished: each move into `done`, plus `working` to `idle` for agents that never say done. `done` then `idle` counts once. |
| `working_s` | Whole seconds spent in `working`, including the current stretch up to when the document was written. |
| `first_seen_ms` | When this pane's agent was first seen (Unix ms). |
| `updated_ms` | When its state last changed (Unix ms). |

An entry disappears when its pane exits. A different agent kind in the same
pane starts a fresh entry. At most the 128 most recently updated agents are
kept. A server restart resets every tally, because terminal ids restart with
it; an upgrade keeps them. The document is rewritten on every state change,
pane exit, and attach, not on a timer: for a live clock, a client adds the
time since it received the value for an agent it sees in `working`.
