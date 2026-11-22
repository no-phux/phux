# Script phux

```sh
# Open a workspace and return the test process's exit status.
phux new --json -s build -c "$PWD" -- sh
phux terminal run --session build -c "$PWD" -- make test
phux ls
```

```sh
# Read or follow a pane. Use the ID from phux ls.
phux snapshot --tail 20 @7
phux watch --json @7
```

```sh
# Run a local handler for each new bell. Event JSON arrives on stdin.
phux on bell --target @7 -- cat
```

```sh
# See what this server supports.
phux api describe terminal.create
```

## More

- [Shell scripting](../../docs/scripting.md): exit codes, JSON, events.
- [Public API](../../docs/public-api.md): launch with a receipt, recover a lost reply.
- [External proofs](./proofs.md): worktree launcher, question inbox, review action.
