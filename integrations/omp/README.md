# @phux/omp

Native Oh My Pi extension for CLI-backed phux terminal and agent tools.

```sh
omp plugin install @phux/omp
omp plugin upgrade @phux/omp   # move to the newest release
```

Requires `phux` on `PATH` and a running local phux server. It registers the
shared phux tool set (list, spawn, snapshot, send keys, paste, run, wait,
agent prompt and wait, resource wait, status, runtime info) plus
`/phux-status` and `/phux-attach`, reports OMP's lifecycle to phux as an
agent session stream, puts mail sent to its pane into the conversation as a
`phux.mail` follow-up (`phux mail wait`, never typing into the pane), and
refuses input or destructive actions against its own pane. See [coding agents](https://github.com/no-phux/phux/blob/main/docs/agents.md).
