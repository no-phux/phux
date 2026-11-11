# @phux/omp

Native Oh My Pi extension for CLI-backed phux terminal and agent tools.

```sh
omp plugin install @phux/omp
omp plugin upgrade @phux/omp   # move to the newest release
```

Requires `phux` on `PATH` and a running local phux server. It registers the
shared phux tool set (list every resource kind, spawn, snapshot, send keys,
paste, run, wait, agent prompt and wait, resource wait, status, runtime info,
and the document reads `phux_documents`, `phux_tree_snapshot`,
`phux_tree_wait`, and `phux_surface_snapshot`, whose frame arrives as an
image; snapshot, wait, send keys and paste act on a terminal only and refuse
any other kind with `wrong_resource_kind`, naming the tool that reads it) plus
`/phux-status` and `/phux-attach`, reports OMP's lifecycle to phux as an
agent session stream, puts mail sent to its pane into the conversation as a
`phux.mail` follow-up (`phux mail wait`, never typing into the pane), and
refuses input or destructive actions against its own pane. See [coding agents](https://github.com/no-phux/phux/blob/main/docs/agents.md).
