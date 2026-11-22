# Native terminal windows

`phux native` displays one server-owned terminal in your terminal emulator.
There is no sidebar, tab rail, status bar, or phux pane layout. Hyprland (or
your window manager) tiles the native windows. The server keeps the shell
running when a client detaches or its window closes.

## Create and reattach

In each native window, run:

```sh
phux native create
```

Each invocation creates a **distinct** terminal in the persistent session
`omarchy`, creating that session if needed. It does not attach an existing
focused terminal. An explicit session, directory, and program are optional:

```sh
phux native create -s omarchy -c ~/work -- bash
phux ls omarchy
phux native attach @3
```

Use the exact `@N` address from the listing to reattach. The client also
prints its address before entering terminal mode. Ctrl-] detaches without
terminating the process. Ctrl-C and Ctrl-Z are delivered to the program;
external HUP, TERM, INT, QUIT, or TSTP signals detach the client and restore
its terminal. Typing `exit` exits the shell; closing a window leaves it
running. A pasted Ctrl-] is ordinary paste data.

Rune sees these same terminals in the same session **when connected to the
same server socket/profile**. Native clients never publish TUI layout or
change session focus. Rune and the CLI can inspect and manage the terminals
normally. The server's existing input leases still apply: attaching does
not steal an exclusive lease from another client. Avoid simultaneously
resizing the same terminal from Rune and a native window: one PTY has one
size, so the last accepted resize wins.

## Try a development build

Build in a feature worktree. Use a private socket and private server state;
do not install over a release binary, upgrade an installed server, or change
a shell startup file to an unreleased command.

```sh
mise exec -- just build
# Run once in a separate shell; keep this shell open while testing.
dev_dir=$(mktemp -d)
printf 'Private test directory: %s\n' "$dev_dir"
env -u PHUX_SOCKET -u PHUX_PROFILE -u PHUX_WS_TOKENS \
  -u PHUX_WS_TLS_CERT -u PHUX_WS_TLS_KEY \
  XDG_STATE_HOME="$dev_dir/state" XDG_CONFIG_HOME="$dev_dir/config" \
  ./target/debug/phux --socket "$dev_dir/phux.sock" server --session omarchy
```

In each test window, substitute the printed directory below:

```sh
PHUX_PLAIN_SHELL=1 ./target/debug/phux --socket /tmp/PRINTED_DIRECTORY/phux.sock native create
./target/debug/phux --socket /tmp/PRINTED_DIRECTORY/phux.sock ls omarchy
./target/debug/phux --socket /tmp/PRINTED_DIRECTORY/phux.sock native attach @3
```

The foreground test server starts with a seed terminal; each native `create`
adds another. Stop only that foreground server when finished. `PHUX_PLAIN_SHELL`
is an escape hatch for the local Omarchy Bash startup workaround, not a phux
setting. The server marks its own shells with `PHUX_TERMINAL_ID`, which
prevents that workaround from nesting a full TUI inside the shell.

## Scope

This initial client supports local Unix sockets and the existing synthesized
VT/raw bootstrap. It reuses phux's transport, stdin parser, structured input,
PTY resize, and terminal restoration code. It validates stream generations
and contiguous output before printing bytes. It uses all available terminal
rows and columns, ignores transient zero sizes, and updates on SIGWINCH.

The emulator renders VT output directly; native checkpoint and state-sync
delta profiles are not advertised. Existing bootstrap and compatibility
emitter limits apply, including image and scrollback fidelity. Host terminal
query responses are consumed by the existing stdin parser; this is not an
arbitrary byte tunnel. No automatic reconnect or input replay is attempted:
reattach explicitly after a connection failure. Creation and attachment are
separate operations: a very short program can finish before attachment, in
which case the client reports a refusal and restores the terminal. The
completed organization record remains, but its live screen may be gone.
This mode is intended for interactive shells and long-running programs.
SIGKILL, SIGSTOP, a machine crash, and terminal emulator failure cannot run terminal cleanup. Server
processes survive client closure, not server termination or reboot.
