# Migrate your config with your agent

`phux migrate` moves the key bindings you already have in tmux, zellij,
Ghostty or herdr, and your tmux settings, into your phux config. phux reads your files and decides
every line it can; your own coding agent (Claude Code, Codex or OpenCode)
writes the result, checks it and reloads phux, in a pane next to you.

```sh
phux migrate
```

## What it reads

| Source | Files |
| --- | --- |
| tmux | the first of `~/.tmux.conf` and `$XDG_CONFIG_HOME/tmux/tmux.conf` (or `~/.config/tmux/tmux.conf`), and every file it `source`s |
| zellij | `$XDG_CONFIG_HOME/zellij/config.kdl` |
| Ghostty | `$XDG_CONFIG_HOME/ghostty/config` and, on macOS, `~/Library/Application Support/com.mitchellh.ghostty/config`, and every `config-file` they load |
| herdr | `$XDG_CONFIG_HOME/herdr/config.toml` |

`--source tmux` (repeatable) reads only the sources you name. With nothing
to migrate, `phux migrate` says so and exits 0.

phux only reads files. It never runs tmux or any other program, and it never
edits your phux config itself: the agent does, and it backs up an existing
file first.

## Two questions first

With a tmux config, `phux migrate` asks two things on the terminal before it
starts the agent. Enter takes the first answer.

- **Beginner or advanced.** Beginner keeps a phux default wherever it already
  does what your tmux did: if you wrote `unbind '"'` because you split with
  `-`, phux has `-` too, so `"` stays. Advanced mirrors tmux as closely as
  phux allows and writes those unbinds. `--advanced` picks it up front.
- **Which prefix.** tmux's default is <kbd>Ctrl</kbd>+<kbd>B</kbd>; phux's is
  <kbd>Ctrl</kbd>+<kbd>A</kbd>. Keep phux's (the default in beginner mode) or
  use tmux's (the default in advanced mode). `--prefix C-b` answers it up
  front.

`--print`, `--json` and runs without a terminal ask nothing: they use
beginner mode and phux's prefix unless `--advanced` or `--prefix` says
otherwise.

## What phux decides, and what the agent decides

For tmux and Ghostty, phux writes a table with one row per line of your
files: the exact phux line to write, "already phux's default", "nothing to
do" (you lose nothing: it keeps working where it is, or phux already behaves
this way), or "not written" with the reason (you lose it). tmux bindings use the same translator as
[`phux config import-tmux`](./tmux-import.md); `-c "#{pane_current_path}"`
is dropped because phux opens new panes in the focused pane's directory
already. A trailing `\; display "..."` is dropped too, so `bind r source-file
~/.tmux.conf \; display "reloaded"` becomes `reload-config`. Ghostty keybinds
become [global bindings](./keys.md#binding-a-key).

tmux settings become phux settings where one does the same job: `mouse`,
`history-limit`, `default-shell`, `status-position` and `set-clipboard`.
Others are "nothing to do" when phux already behaves that way (`mode-keys vi`,
`renumber-windows on`, `escape-time`, `focus-events`, `default-terminal`), or
"not written" when you lose them (`base-index 1`: phux numbers tabs from 0;
`status-left` and the other status formats: phux's bar is built from
[widgets](./status-bar.md)).

Ghostty settings (font, opacity, padding, cursor, theme) are "nothing to do":
Ghostty keeps using them around phux.

Some lines are never written:

- Any binding on Cmd. phux key bindings have no Cmd modifier, and the macOS
  system shortcuts (<kbd>Cmd</kbd>+<kbd>Q</kbd>, <kbd>Cmd</kbd>+<kbd>H</kbd>,
  <kbd>Cmd</kbd>+<kbd>M</kbd>, <kbd>Cmd</kbd>+<kbd>Tab</kbd>,
  <kbd>Cmd</kbd>+<kbd>Space</kbd>, <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>3</kbd>/<kbd>4</kbd>/<kbd>5</kbd>,
  <kbd>Ctrl</kbd>+<kbd>Cmd</kbd>+<kbd>Q</kbd>) are never bound.
  A Ghostty Cmd binding still works in Ghostty, but on Ghostty's own tabs and
  splits, not phux's. Rune, phux's Mac app, has Cmd shortcuts built in.
- tmux copy-mode bindings, plugins and commands no phux action matches.

zellij and herdr lines, and Ghostty lines phux cannot translate with
certainty (key sequences, actions with no clear counterpart), go to the
agent as "translate these yourself", with the list of phux actions it may
bind. A binding with no phux action is reported, never approximated.

When two sources bind the same key differently, the agent asks you once per
pair of sources which one wins. It never decides alone.

The agent then runs `phux config check` until the file is clean, `phux
config reload`, and `phux config keys --json`, and checks from that output
that every key you had still runs the same action. It reports Migrated,
Replaced phux defaults, Conflicts between sources, Could not migrate (one
line each: you lose these) and Nothing to do (grouped by reason). Every line
of your files lands in one of them. A line phux marked "not written" that the
agent thinks a phux action matches is listed under Possible misses instead
of being written, so the translator can be fixed. If its sandbox blocks the
reload, it asks you once, or tells you the reload is pending.

## Where the agent runs

- `--agent claude|codex|opencode` picks the agent. Without it, phux starts the
  one that is installed, asks when there are several, and with `--json` or no
  terminal fails with the list.
- Inside a phux pane, the agent opens in a split to the right.
- Anywhere else, it starts in a new session named `migrate` and attaches.
  Without a terminal, phux prints the `phux attach` command instead.

The agent's working directory is your phux config directory, created if
needed.

## Any other agent

`phux migrate --print` prints the prompt and starts nothing. Paste it into
any agent:

```sh
phux migrate --print | pbcopy
```

## JSON

`phux migrate --json` prints one document after launching:

```json
{
  "schema_version": 1,
  "sources": [
    {
      "name": "tmux",
      "files": ["/Users/me/.tmux.conf", "/Users/me/.tmux/keys.conf"],
      "found": { "bindings": 13, "unbindings": 1, "settings": 5 }
    }
  ],
  "agent": "claude",
  "config_path": "/Users/me/.config/phux/config.toml",
  "launched": { "pane": "@12" },
  "prompt_bytes": 15873,
  "mode": "beginner",
  "prefix": { "phux": "C-a", "tmux": "C-b", "chosen": "C-a" }
}
```

- `sources[].name` is `tmux`, `zellij`, `ghostty` or `herdr`; `files` are
  absolute; `found` counts binding, unbinding and setting lines.
- `mode` is `beginner` or `advanced`. `prefix.chosen` is the prefix the
  migration writes or keeps; `prefix.tmux` is tmux's, or `null` without a
  tmux config. An onboarding that asks its own questions passes the answers
  as `--advanced` and `--prefix`.
- `launched` is `{ "pane": "@N" }` for a split, `{ "session": "migrate",
  "pane": "@N" }` for a new session, and `null` when nothing started.
- With nothing to migrate, `sources` is empty, `agent` and `launched` are
  `null`, and `prompt_bytes` is 0.
- `--print --json` starts nothing: `launched` is `null`, `agent` is the one
  `--agent` names or the only one installed (else `null`), and the document
  adds `prompt`, the full text.

Failures use the [JSON error contract](./scripting.md#json-output-and-errors):
`usage_error` (exit 2) when the agent is not installed, none is, or several
are and `--agent` is missing.
