# phux plugin index

Plugins run as the user and aren't sandboxed (see docs/plugins.md).

Generated 2026-01-15T00:00:00Z.

| Name | Description | Kinds | Repository | Subdirectory | Stars | Updated | License | Install |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Theme | A theme in a subdirectory. | themes | acme/pack | plugins/theme | 1 | 2026-04-04T00:00:00Z | - | `phux plugin install https://github.com/acme/pack` |
| Widgets | Pane and action tools. | actions, panes | acme/widgets | - | 12 | 2026-03-03T12:00:00Z | MIT | `phux plugin install https://github.com/acme/widgets` |
| Demo | Example plugin from the checkout. | agents | fixture/demos | examples/plugins/demo | 4 | 2026-02-02T00:00:00Z | Apache-2.0 | `phux plugin install https://github.com/fixture/demos` |

## Skipped

- `acme/baddirs` — invalid plugin directory: ../escape
- `acme/broken` — invalid manifest: acme/broken/phux-plugin.toml: event `nope` listens for `on-startup`, which is not a hook event; it will never fire
- `acme/empty` — no valid manifest
- `acme/forked` — fork
- `acme/old` — archived
- `acme/pack` (`plugins/missing`) — no valid manifest
- `acme/secret` — private
