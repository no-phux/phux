# Script phux

Use the shell for a few commands. Use the [public API](../../docs/public-api.md)
when you need exact identities, schemas, or recovery after a missing reply.
The [external composition proofs](./proofs.md) deliberately exercise failure
paths; you don't need to copy them to start scripting.

## Open a project

```sh
phux new --json -s build -c "$PWD" -- sh
phux spawn --target build -- make test
```

This creates a named session with a shell, then starts tests beside it,
without taking over your terminal. Use `phux ls` to see what is running.

## Start a process in that session

Use `jq` to get the session's durable key and send argv as JSON. This puts
`make test` in a new terminal; it does not type into an existing shell.

```sh
set -eu
session=$(phux ls --json | jq -er '.sessions[] | select(.name == "build") | .key')
key=$(openssl rand -hex 16)
request=$(jq -n --arg session "$session" --arg key "$key" \
  '{session:$session, idempotency_key:$key, argv:["make","test"]}')
printf '%s\n' "$request" | phux api call terminal.create --request-json -
```

The reply is a receipt for launch intent, not the exit status of `make`.
If you lose the reply, use `terminal.operation` with the same `session` and
`idempotency_key`; don't start another process to find out what happened.

## Read or follow the work

```sh
phux snapshot --tail 20 @7
phux watch --json @7
```

Replace `@7` with the current pane shown by `phux ls`. A live handle is
convenient for a human; keep the durable key from the receipt when your
program needs to look up that resource later.

## Discover what you can call

```sh
phux api list | jq -r '.operations[].name'
phux api describe terminal.create
```

See [scripting](../../docs/scripting.md) for output, exit codes, and event
recovery. See [external composition proofs](./proofs.md) for a worktree
launcher, agent-question inbox, and contextual review action.
