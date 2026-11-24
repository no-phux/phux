---
group: automate
order: 0
---

# Using phux as an agent

The shortest path from nothing to a program driving a phux pane.

```sh
curl -fsSL https://phux.sh/install | sh     # phux and phux-mcp into ~/.local/bin
phux server --ensure                        # start or reuse a server, then return
phux new --json -s agent -c "$PWD" -- claude
```

Other installs (Homebrew, source, WSL) are in [install](./install.md). To
change phux itself, read [AGENTS.md](../AGENTS.md). `phux skill` prints the
guide built into the binary you have.

## Drive a pane

`phux new --json -s NAME` prints `terminal_id`; address that pane as `@N`.
The program runs under the server's `$SHELL -c`, so it must be on that
shell's `PATH`.

```sh
doc=$(phux new --json -s agent -c "$PWD" -- claude)
pane="@$(printf '%s\n' "$doc" | jq -r .terminal_id)"
phux wait --idle 1500 --timeout 30 "$pane"
phux send-keys "$pane" 'fix the login redirect and run the tests' Enter
phux wait --until 'tests passed' --timeout 600 "$pane"
phux snapshot --json --tail 200 --unwrap "$pane"
phux kill --yes "$pane"
```

- Flags go before the target; arguments after it are keys or text.
- `send-keys --json` and `paste --json` return a receipt: the bytes reached
  the pane, not that the program read them. If delivery is unknown, read
  the pane before sending again.
- For a recognized agent, `phux agent prompt --json "$pane" 'TEXT'` sends one
  line with a receipt ([driving an agent](./agents.md#driving-an-agent-in-a-pane)).
- `phux run "$pane" CMD` types into a shell and exits with the command's
  status. It refuses (exit 2) when the foreground program is not a shell.
- `kill`, `detach` and `rm` refuse without `--yes` when stdin is not a
  terminal.

[Shell scripting](./scripting.md) covers waiting, events, and reading
screens in full.

## Call phux from a program

Pass `--json` on any command whose `--help` lists it. Success is a document
with `schema_version` on stdout. Failure leaves stdout empty and writes one
JSON object to stderr; branch on `error.code`
([JSON output and errors](./scripting.md#json-output-and-errors)).
`phux status`, `phux ls --all` and `phux doctor` print their document even
when no server runs.

Exit codes are in [the reference](./reference/exit-codes.md). One trap:
`phux agent answer` exiting 3 (`answer_delivery_unknown`) means part of the
answer may have arrived. Read the pane; never resend.

Other ways in:

- **MCP:** register `phux mcp` as a stdio server; `phux mcp --schema` lists
  the tools ([MCP](./agents.md#mcp)).
- **Durable creation:** `phux terminal create` and `phux api list` for
  integrations that must survive lost replies
  ([durable terminal creation](./scripting.md#durable-terminal-creation)).
- **The socket:** framing and handshake in [protocol](./protocol.md), every
  frame in [`wire-schema.json`](./spec/wire-schema.json); `phux runtime-info
  --json` prints this binary's protocol and capabilities.
- **Skills:** `phux skill setup` points each installed agent at `phux skill`
  ([skill setup](./agents.md#skill-setup)).

## Run without a terminal

`phux`, `phux attach`, and `phux new` without `--json` need a terminal on
stdin and stdout; without one they exit 1. Scripts use `phux new --json`.
`phux ls` and `phux spawn` never start a server; `phux server --ensure` and
`phux new` do.

Inside a pane, `PHUX_TERMINAL_ID` is your own pane and `PHUX_SOCKET` its
server. Don't send keys to your own pane. Attaching back to the same server
from inside it is refused (exit 2) unless `PHUX_ALLOW_NESTED` is set.
