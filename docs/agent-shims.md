# Agent launch shims

For integration installation, see [Agent setup](./agents.md#agent-setup).

## Claude Code from any terminal

`phux agent install-claude` makes a plain `claude` command start inside a
phux pane with its hooks wired up, even when you run it from a terminal
outside phux:

```sh
phux agent install-claude
```

It puts a small wrapper named `claude` in `~/.local/share/phux/shims` and
adds one marked block to your shell's rc file (`~/.zshrc`, `~/.bashrc`, or
fish's `config.fish`) that puts that directory first on `PATH`. It never
replaces the real `claude`. Pass `--shell` or `--real PATH` when it guesses
wrong. `phux agent uninstall-claude` removes both, and `phux doctor` reports
when the wrapper is out of date.

## Codex, OpenCode and pi from any terminal

`phux agent install-shim` does the same for the other agents, one at a time
or all at once:

```sh
phux agent install-shim --all              # every agent on PATH
phux agent install-shim --agent codex      # just one (repeatable)
phux agent uninstall-shim                  # remove every shim
```

`--all` covers `claude` too (with the same wrapper as `install-claude`) and
skips agents that are not installed. Codex, OpenCode and pi get a
launch-only wrapper: it opens the agent in a phux pane and declares its
identity, and phux's screen-detection rules report its state. One-shot runs
such as `codex exec`, `opencode run`, `pi -p`, `--help`, or any run with
piped input or output go straight to the real binary. All the wrappers
share one rc block, which leaves with the last of them. Rune's first-run
Agents step offers the same thing as an opt-in card.
