# Using phux as an agent

phux is a terminal multiplexer whose server you can drive from code. Start a server, create a session running your agent, and read it back:

```sh
curl -fsSL https://phux.sh/install | sh
phux server --ensure
phux new --json -s agent -c "$PWD" -- claude
```

To work on phux itself, read [AGENTS.md](../AGENTS.md). Install options,
scripting, agent state, and the socket are in [install](./install.md),
[shell scripting](./scripting.md), [coding agents](./agents.md), and
[protocol](./protocol.md).

`phux skill quick`, `phux --help`, `phux help targets`, and
`phux help exit-codes` describe the binary you have installed.

## Install

```sh
curl -fsSL https://phux.sh/install | sh
```

The script installs `phux` and `phux-mcp` into `~/.local/bin`. Open a new
shell when that directory is not on `PATH` yet.

Homebrew, from the `no-phux/tap` tap:

```sh
brew trust --tap no-phux/tap
brew install no-phux/tap/phux
```

Homebrew before version 6 does not ask you to trust the tap; skip the
first line. Update with `brew upgrade no-phux/tap/phux`. See
[Homebrew](./install.md#homebrew).

From a checkout of this repository:

```sh
cargo install --locked --path crates/phux
cargo install --locked --path crates/phux-mcp
```

A binary run from `target/` uses the `dev` profile and its own socket.
`cargo install` puts `phux` on `PATH` under the `default` profile. See
[build from source](./install.md#build-from-source).

On Windows, the CLI, server, and terminal UI run under WSL 2 only.
PowerShell launchers call into that WSL install. Desktop 0.50.0 and later
also provide a native Windows app, backed by the WSL coordinator. See
[Windows (WSL)](./install.md#windows-wsl) and
[requirements](./install.md#requirements).

The desktop app (Phux) is separate. The CLI is what scripts drive. See
[desktop app](./install.md#desktop-app).

```sh
phux --version
phux doctor
```

## Start the server

`phux` and `phux attach` open the terminal UI. A script starts the server
in the background and returns:

```sh
phux server --ensure
```

Exit 0 means the selected socket accepts a connection. With no extra
flags the success path prints nothing. `--json` prints `schema_version`,
`running`, `socket`, `disposition` (`reused`, `joined`,
`supervised_started`, or `daemon_started`), `cli_version`, and
`server_log`. Failure exits 1.

`phux new` starts a server when none is running. `phux spawn` does not:
with no server it exits 1. Bare `phux server` stays in the foreground
until Ctrl-C.

## Create a session and a pane

Create a named session, run a program in its first pane, and do not
attach:

```sh
phux new --json -s agent -c "$PWD" -- claude
```

`--json` requires `-s NAME`. The document is `schema_version`, `session`,
and `terminal_id`. Address the pane as `@N`, where N is `terminal_id`.
A name already in use is an error; pick another name, or end the old
session with `phux kill --yes NAME` before creating it again. A session
you name stays after its last process exits, until `phux rm --yes` or
`phux kill --yes` deletes it.

The program is started as `$SHELL -c`. A server started from a terminal
does not use a login shell. A server started by `phux service install`
does (`PHUX_SERVICE_MANAGED`). The program has to be on that shell's
`PATH`.

Add a pane to the server's most recently active session:

```sh
phux spawn --json -c "$PWD" -- claude
```

`--json` includes `terminal_id`. Put the program after `--`.

## Send input

Flags go before the target. Arguments after the target are keys.

```sh
phux send-keys @7 'fix the login redirect' Enter
```

`Enter`, `Tab`, `Escape`, `Up`, `C-c`, and `M-x` are key names. Any other
argument is literal text. A literal run immediately before `Enter` is
delivered as a paste, then the real Enter.

Paste inserts and does not submit:

```sh
printf '%s\n' 'line one' 'line two' | phux paste @7
phux send-keys @7 Enter
```

With `--json` before the target, `send-keys` and `paste` print a receipt:
`schema_version`, `sent`, `terminal_id`, and `events`, the number of input
events the server acknowledged. The receipt means the bytes reached the
pane's input, not that the program read them. When a send fails partway,
`error.recovery.effect_certainty` is `unknown`, so read the pane before you
send the same text again.

When the pane is already a recognized agent, one line goes out with a
receipt:

```sh
phux agent prompt --json @7 'fix the login redirect'
```

The text has to be a single line. An OK means the bytes reached the
pane's input queue. When delivery comes back unknown, read the pane
before any further send to that pane. Two prompts at once to the same
pane are refused. See
[driving an agent in a pane](./agents.md#driving-an-agent-in-a-pane).

`phux run` types a command into a POSIX shell, waits, and exits with that
command's status (125 when `--timeout` fires). It exits 2 when the
foreground program is not a shell. Prompt an agent with `send-keys` or
`agent prompt`.

## Read output and list sessions

```sh
phux snapshot --json --tail 200 --unwrap @7
phux snapshot --json --scrollback 200 @7
phux ls --json
phux status --json
```

`snapshot` does not attach or resize. `--tail N` returns the last N rows
(history, then the screen). Bare `--tail` returns 80. `--scrollback N`
adds N rows of history above the screen; in JSON that history is the
`scrollback` field. Bare `--scrollback` requests the retained history.
`--unwrap` joins soft-wrapped rows.

`phux ls` does not start a server. With none running it exits 1;
`--json` leaves stdout empty and writes one error object to stderr.
`phux status --json` with no server prints `"running": false` on stdout
and exits 1. `phux ls --all --json` still prints its host document,
naming a down host `no server running`, and exits 0. `phux doctor --json`
prints its report either way; a stopped server is a warning.

## Detach and kill

```sh
phux detach --yes agent
phux kill --yes @7
phux kill --yes agent
```

`detach` disconnects clients attached to that session. The panes keep
running. `kill` of a pane ends that pane. `kill` of a session name ends
every pane in it and deletes the session. `phux kill --server` stops the
server process and does not ask for confirmation.

`detach`, `kill`, and `rm` ask on a terminal. When stdin is not a
terminal they exit 2 and send nothing, unless you pass `--yes`.

A person attaches from a terminal:

```sh
phux attach agent
```

`attach` requires a TTY on stdin and stdout. On a server that is already
running, an unknown name is an error; `attach` does not create the
session.

## Run an agent and read its output

Run a coding-agent CLI in a session, send one prompt, print a JSON
capture, and leave the session running. Replace `claude` with the agent
executable on the pane shell's `PATH`. `jq` is not part of phux. Use a
free session name.

```sh
phux server --ensure
doc=$(phux new --json -s agent -c "$PWD" -- claude)
pane="@$(printf '%s\n' "$doc" | jq -r .terminal_id)"
phux wait --idle 1500 --timeout 30 "$pane"
phux send-keys "$pane" 'fix the login redirect and run the tests' Enter
phux snapshot --json --tail 200 --unwrap "$pane"
```

`new` returns when the pane exists; `wait --idle` holds until the screen
settles. To wait for a line only that program prints, run
`phux wait --until TEXT --timeout SECS` with the target after the flags.
`--output-only` skips lines the shell marked as input. A timeout exits
124. See [waiting for output](./scripting.md#waiting-for-output).

To start a recognized integration in a shell pane that already exists,
and return once phux has seen it: `phux agent start NAME --kind KIND
--target @N`. A kind with no detection manifest is refused. `phux launch
--list` lists integrations from enabled plugins.

## Call phux from a program

Pass `--json` on commands whose `--help` lists it. The document carries
`schema_version`. On failure, stdout is empty and stderr is one JSON
object; branch on `error.code` (except `status`, `ls --all` and
`doctor`; see above). See
[JSON output and errors](./scripting.md#json-output-and-errors).

Exit codes, from `phux help exit-codes`: `0` success, `1` failure (no
server, no such target, or the command failed), `2` a usage error or a
refused request, `3` the selector was resolved against a partial view of
the fleet, `124` a wait timed out, `125` `phux run` timed out. The same
table is [exit codes](./reference/exit-codes.md).

Clients on this machine dial a Unix socket: `--socket`, else
`$PHUX_SOCKET`, else the path in [socket](./reference/files.md#socket).
Framing and the handshake are in [protocol](./protocol.md). Every frame
and field is in [`wire-schema.json`](./spec/wire-schema.json), generated
from the codec.

Rust code in this repository speaks that socket through `phux-protocol`,
`phux-client-core`, and `phux-client-runtime`. `phux-client-core` and
`phux-client-runtime` set `publish = false`. [Embedding](./embedding.md)
is how another app draws a terminal.
[`clients/phux-web`](../clients/phux-web) is a client written beside the
Rust crates. This repository ships no Python or JavaScript package for
driving phux.

MCP over stdio, same machine:

```sh
phux mcp --schema
```

`phux mcp` forwards its arguments to a `phux-mcp` binary beside `phux`
or on `PATH`. The install script, Homebrew, and both `cargo install`
lines above install that binary. When it is missing, `phux mcp` exits
127 and prints `could not find the bundled phux-mcp executable`.
Register the stdio server as `phux mcp`. Each tool mirrors a CLI
command. See [MCP](./agents.md#mcp) and [parity](./reference/parity.md).

```sh
phux skill
phux runtime-info --json
```

`phux skill` prints the guide compiled into that binary. `runtime-info`
prints the protocol and capabilities of that binary.

## Run without a terminal

`phux` and `phux attach` require a terminal on both stdin and stdout.
Without one they exit 1 and do not open the UI. `phux new` without
`--json` attaches, so it has the same requirement. `phux new --json`
creates the session and returns.

Inside a pane, `PHUX_TERMINAL_ID` is that pane's number and `PHUX_SOCKET`
is its server. Don't send keys to your own pane. `phux`, `phux attach`, and
an attaching `phux new` refuse to attach back to that same server unless
`PHUX_ALLOW_NESTED` is set (exit 2).

Put flags for `send-keys` and `run` before the target: words after it
are keys or the command. `phux wait` accepts flags before or after the
target; spell a bare `--tail` out (`--tail 80`) when the target follows
it. `send-keys`, `paste`, `detach`, `kill`, and `attach` do not accept
`--json`. `phux ls` and `phux spawn` leave the server unstarted.
`phux server --ensure` and `phux new` start one.
