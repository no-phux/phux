# phux reference

Every page in this directory is generated from the `phux` binary.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Run `just docs-gen` after changing a command, setting, or other surface listed here; a unit test fails until the pages match the binary.

| Page | Contents |
|---|---|
| [cli.md](cli.md) | Every non-hidden `phux` invocation path with its flags, defaults, and help text. |
| [config.md](config.md) | Every `config.toml` section, the scalar defaults, and the annotated default config. |
| [keys.md](keys.md) | Every key phux binds out of the box, grouped as the which-key popup shows them. |
| [actions.md](actions.md) | Every dispatcher action with its parameters, description, and palette placement. |
| [widgets.md](widgets.md) | Every registered status-bar widget kind with its options and defaults. |
| [hooks.md](hooks.md) | Every hook event the server fires, with its context keys, environment projection, and matching rules. |
| [exit-codes.md](exit-codes.md) | Every exit code the binary uses and what each one means. |
| [environment.md](environment.md) | Every environment variable the binary reads and what each one does. |
| [files.md](files.md) | The symbolic path rule for every file phux reads or writes: socket, config, logs, TLS material, tokens, bug reports, and the ideas spool. |
| [deprecations.md](deprecations.md) | Every deprecated spelling, its replacement, and its removal release. |
| [kinds.md](kinds.md) | Every resource kind, its methods and events, and the verb each frame and command needs. |
| [parity.md](parity.md) | Every MCP tool, the CLI verb it mirrors, how it runs, and its annotations. |
