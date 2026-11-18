# Script phux

```sh
# Open a workspace and run tests beside the shell.
phux new --json -s build -c "$PWD" -- sh
phux spawn --target build -- make test
phux ls
```

```sh
# Read or follow a pane. Use the ID from phux ls.
phux snapshot --tail 20 @7
phux watch --json @7
```

```sh
# See what this server supports.
phux api describe terminal.create
```

## More

- [Shell scripting](../../docs/scripting.md): exit codes, JSON, events.
- [Public API](../../docs/public-api.md): launch with a receipt, recover a lost reply.
- [External proofs](./proofs.md): worktree launcher, question inbox, review action.
