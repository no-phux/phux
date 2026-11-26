---
group: customize
order: 0
---

# Configuration file

How `config.toml` is found, layered, checked, and applied. Every section and
setting, with its default, is in the [config reference](./reference/config.md).

The file is a sparse overlay: a key you set replaces the shipped value, and
everything else keeps the default. You don't need one to start.

## Coming from another multiplexer

`phux migrate --source tmux` reads your tmux config and translates every key
binding it can into phux config lines. It then starts your coding agent, which
writes those lines into your config, runs `phux config check` and `phux config
reload`, and reports what it could not translate. Migrate asks when your tmux
prefix differs; `--prefix C-b` picks tmux's default up front.

```sh
phux migrate --source tmux
```

[`phux config import-tmux`](./reference/cli.md#phux-config-import-tmux)
reads a running tmux and writes a layer without an agent.
[`phux migrate`](./reference/cli.md#phux-migrate) also reads zellij, Ghostty,
herdr, and Rex configs. Neither command reloads a running server itself; the
agent `phux migrate` starts runs `phux config reload`.

`import-tmux --apply` writes `tmux-import.toml` beside your config and
prepends it to `extends`, so your own settings still win. It refuses to
overwrite an earlier import. To undo, remove it from `extends` and delete
the file. Preview first:

```sh
phux config import-tmux > tmux-preview.toml && phux config check tmux-preview.toml
```

[Getting started](./getting-started.md#coming-from-tmux) lists what differs
from tmux.

## Where it lives

`$XDG_CONFIG_HOME/phux/config.toml`, else `~/.config/phux/config.toml`.
`phux config path` prints the resolved path. No flag points phux at another
file; set `XDG_CONFIG_HOME` for one shell to try a config in isolation.

Each process reads the file on its own machine. A client attached to a remote
server uses its own key bindings, theme, and status bar; `[defaults]`, hooks,
and `[policy]` come from the server's machine.

```sh
phux config init        # commented copy of every default; changes nothing until you uncomment
phux config show        # merged settings in effect (--default: shipped; --layers: who set each key)
phux config keys        # every key binding and where it came from
```

`phux config init --distro starter` adds an `extends` line for the starter
[distribution](./distros.md).

## When a change takes effect

phux never watches the file. `phux config reload` (or the `reload-config`
action) validates it first and signals nothing if it is invalid. A client
whose own re-read fails keeps its previous settings and shows a toast.

| Settings | Take effect |
| --- | --- |
| Key bindings, which-key, theme, status bar, `[chrome]`, `[notifications]`, clipboard policy, `[[sessions]]` colours, plugin actions, panes and sidebar sections | On `phux config reload` |
| `[[satellites]]`, `[[uplink]]` | On `phux config reload`, by the server |
| `[sidebar]`, `mouse`, `[labs] predictive-echo` | On the next attach |
| The rest of `[defaults]`, `[[hooks]]`, plugin event hooks, `[policy]`, `[voice]`, `[limits]`, `[[restore.program]]` | When the server starts |

`phux upgrade` restarts the server in place without closing panes, which is
how a running server picks up start-time settings. The Settings page shows
each setting's timing.

## Checking a file

`phux config check [PATH]` reports every problem in one pass with its key
path, and names the layer file when the problem comes from one. It exits 0
clean, 1 with findings, 2 when it cannot run; `--json` makes it a CI gate for
dotfiles. A missing default file is clean.

```text
$ phux config check
sidebar.enabledd: unknown key: unknown field `enabledd`, expected one of `enabled`, `width`, ...
keybindings.prefix-table.q: unknown name: unknown action `kill-pain` (did you mean `kill-pane`?)
2 problems
```

A broken file never locks you out. When it fails to load, attach runs on the
shipped defaults and shows the error on the status bar; a failed reload keeps
the config you had. A server refuses to start on it, so check before you
restart one. One bad key binding disables only itself.

Editor completion (taplo, Even Better TOML) comes from the schema:

```toml
#:schema https://raw.githubusercontent.com/no-phux/phux/main/docs/spec/config.schema.json
```

## Layers

`extends` stacks files. Order: shipped defaults, each extended file in the
order listed (recursively), then the file that extends them. Later wins.

```toml
extends = ["base", "work.toml"]

[status]
right-append = [{ kind = "cwd" }]
```

- An absolute path is used as written; a relative path or any `.toml` name is
  relative to the naming file; a bare name like `base` is
  `layers/base.toml` beside it.
- Tables merge key by key. An array replaces the one beneath it; spell the
  key `<name>-append` to add to the inherited array instead. You can't set
  both in one table.

For dotfiles, keep shared settings in `layers/base.toml` and start each
machine's `config.toml` with `extends = ["base"]`. Keep `[[remote]]`,
`[[connector]]`, `[[uplink]]`, and `[[satellites]]` out of the shared layer:
`phux host add` and friends write them per machine, pointing at token files
that exist only there.

## Settings page

<kbd>C-a</kbd> <kbd>S</kbd> lists every setting with its value, default, the
layer that set it, and when a change applies. Edits change one line of your
`config.toml`, keep comments, refuse an edit that would make the file
invalid, then reload. <kbd>Delete</kbd> removes your override; <kbd>C-z</kbd>
undoes. A value set in an extended file names that file instead.

## Rune

The Rune desktop app reads `[rune]` at launch. A key set here wins over the
choice Rune saved; a key left out stays Rune's. **Copy Settings as TOML** in
Rune's palette copies your current choices as this table. Key names and
values are in the [config schema](./spec/config.schema.json) (`RuneCfg`).

```toml
[rune]
theme = "nord"                    # "" follows the system
font-family = ["Berkeley Mono", "Symbols Nerd Font"]
tab-bar = "bottom"                # "auto" (default), "top", "bottom", "hidden"
quick-terminal = "ctrl+backquote" # global drop-down terminal; "none" is off
chief = "staff"                   # session the right-Cmd tap prompts; "none" is off
ssh-agent = "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"
```

`quick-terminal` shows a session named `quick` over the front app; `phux
attach quick` reaches it too. `ssh-agent` matters because an app opened from
the Dock doesn't see your shell's `SSH_AUTH_SOCK` ([SSH](./ssh.md)). The
right-Cmd tap is the [chief of staff](./acp.md). `[rune.keys]` binds Rune's
shortcuts ([Key bindings](./keys.md#rune-shortcuts)).

### Markdown viewer

Desktop Markdown documents render through the bundled tmd by default;
**Source** and **Rendered** switch views. To replace the renderer, give an
argv (not a shell command):

```toml
[rune]
markdown-viewer = ["/absolute/path/to/renderer", "--width", "{width}"]
# markdown-viewer = []  # source only
```

The renderer reads UTF-8 Markdown on stdin and writes plain UTF-8 to stdout.
Only `{width}` expands. It runs locally, even for remote documents, under a
three-second deadline and a 2 MiB output cap, falling back to source on
failure. It is trusted local configuration, not a sandbox: pick one that
never executes document code or fetches assets.

## Developer mode and labs

A lab is a feature that ships off, or decided for you, until it is ready.
`[labs]` holds your choices by lab id. The server, `phux attach`, Rune and
the CLI read the same file, so one choice reaches every part of phux that
acts on that lab; its scope (`server`, `client` or `system`) says which.

```toml
[developer]
mode = true             # show Labs in Settings, in every client

[labs]
predictive-echo = true  # absent: the lab's default
```

Developer mode only shows the switches: the terminal UI's Settings page and
Rune's Settings list Labs while `[developer] mode` is on. Turning it off
hides them and keeps every lab you set. From a shell:

```sh
phux config labs list            # every lab, its state and scope
phux config labs set predictive-echo on    # on, off or default
phux config labs developer on
```

A client-scoped lab applies on each client's next attach (Rune applies it
at once); a server-scoped one on `phux config reload` or the next server
start. A lab that is removed from phux still loads from an old file;
`phux config check` names it.

## Predictive echo

`[labs] predictive-echo` draws your typing before the server echoes
it, in `phux attach` and in Rune. It replaces `[experimental]
predictive-echo`, which is still read when `[labs]` leaves it unset. Unset, it is on for another machine's
terminals and off for this computer's. It cannot see when a program turns
echo off, so a typed password can flash on your screen for up to about five
seconds; nothing extra is sent. Set it to `false` before typing secrets on a
shared screen, then reattach or reopen Rune.

## Tailnet owner devices

With [Tailscale](https://tailscale.com), your own devices can reach this
machine with no pairing:

```toml
[policy]
mode = "paired"
tailnet-owner = true
```

A device connecting to the QUIC listener on this machine's tailnet address is
admitted with a paired device's access when the local Tailscale client says
it belongs to the same Tailscale user. Never on a LAN or public listener,
never over WebSocket. Tagged devices, devices shared in from another tailnet,
other users, and any device presenting a client certificate still need `phux
pair`. If Tailscale can't answer, or this machine is tagged, nobody gets in
this way. Each connection is audited as `tailnet:<login>`
([Security](./security.md)). Restart the server after changing it.
