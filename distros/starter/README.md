# starter

A config layer that enables the example plugins under
[`examples/plugins`](../../examples/plugins). Everything else the old
"herdr" starter distro used to set is now the shipped default, so a bare
`phux` already behaves that way; what remains here is the one thing an
embedded default cannot express, a `[[plugins-append]]` list with relative
manifest paths. Install with `phux config init --distro starter`.

```sh
phux config init --distro starter
```

That scaffolds `~/.config/phux/config.toml` with one active line:

```toml
extends = ["/absolute/path/to/distros/starter/starter.toml"]
```

followed by the fully-commented shipped defaults. Nothing is copied out
of herdr: the layer file stays authoritative, so pulling a newer phux
checkout updates it for every config that extends it.

Already have a config? Add the `extends` line yourself (top-level, any
config file). See [configuration: layers](../../docs/config.md#layers).

The bundled name `starter` resolves through, in order: `$PHUX_DISTROS_DIR`,
`$XDG_DATA_HOME/phux/distros` (default `~/.local/share/phux/distros`),
then the repo checkout's `distros/` directory. A path (`--distro
./distros/starter` or `--distro ./distros/starter/starter.toml`) works from
anywhere. `--distro herdr` still resolves as an alias of `starter`.
Configs that already `extends` the old `distros/herdr/herdr.toml` path
keep loading: that file remains as a compatibility stub, and the loader
rewrites a missing herdr path to starter when the new file is there.

## What you get

- **Plugins, additively.** `[[plugins-append]]` wires
  `examples/plugins/continuum` (workspace autosave/restore) and
  `examples/plugins/agent-tools` (bench helpers) without erasing plugin
  entries from your own config or another layer. Their actions appear in
  the command palette (`prefix Space`) as `plugin:` rows. To drop them,
  assign a plain `plugins = [...]` in your config - replacement wins over
  inherited appends.
- **Agent identity.** `agent-tools` ships integration templates whose
  `[launch]` command runs `claude`/`codex`/`gemini` through
  `scripts/phux-agent-wrap.sh`. The wrapper writes a `phux.agent/v1` L3
  record (ADR-0040) - a first-class name + kind that the sidebar's
  `agents` section and `phux agent list` prefer over the OSC-title
  substring heuristic that false-positives on titles like
  `vim CLAUDE.md` - and clears it on exit, pinned to the wrapper's own
  pane. Because this layer enables `agent-tools`, `phux launch
  claude-code` (or `codex` / `gemini-cli`) opens a self-identifying pane
  end-to-end: the launch executor (ADR-0042) runs the template `[launch]`
  command and the server injects `PHUX_TERMINAL_ID` so the wrapper
  self-targets with no alias. `phux launch --list` shows the bundled
  integrations. See `examples/plugins/agent-tools/README.md` section
  "Automatic agent identity" for the manual-activation fallback, the
  required pane-targeting, and why live working/blocked state still needs
  a separate signal feed.

## Overriding

Your `config.toml` merges on top of the layer, key by key:

```toml
extends = ["/absolute/path/to/distros/starter/starter.toml"]

[keybindings]
which-key-delay-ms = 800        # slow the popup back down

[theme]
accent = "magenta"              # replace one slot, keep the rest
```

`phux config show` prints the effective result of the whole stack, and
`phux config show --layers` says which layer set each key.

## Layout notes

Relative `manifest` paths in a layer resolve against the layer file's
own directory, which is what lets this layer reference the sibling
`examples/plugins/` tree. If you copy `distros/starter/` out of a phux
checkout, copy those two plugin directories too and update the
`[[plugins-append]]` paths in `herdr.toml`.
