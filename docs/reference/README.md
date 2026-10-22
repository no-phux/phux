# All reference pages

Every page here is generated from the `phux` binary, so it matches the code the rest of the docs describe.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

| Page | Contents |
|---|---|
| [Command line](cli.md) | Every non-hidden `phux` invocation path with its flags, defaults, and help text. |
| [Config settings](config.md) | Every `config.toml` section, the scalar defaults, and the annotated default config. |
| [Default key bindings](keys.md) | Every key phux binds out of the box, grouped as the which-key popup shows them. |
| [Actions](actions.md) | Every dispatcher action with its parameters, description, and palette placement. |
| [Status bar widgets](widgets.md) | Every registered status-bar widget kind with its options and defaults. |
| [Hook events](hooks.md) | Every hook event the server fires, with its context keys, environment projection, and matching rules. |
| [Exit codes](exit-codes.md) | Every exit code the binary uses and what each one means. |
| [Environment variables](environment.md) | Every environment variable the binary reads and what each one does. |
| [File locations](files.md) | The symbolic path rule for every file phux reads or writes: socket, config, logs, TLS material, tokens, bug reports, and the ideas spool. |
| [Deprecations](deprecations.md) | Every deprecated spelling, its replacement, and its removal release. |
| [Resource kinds](kinds.md) | Every resource kind, its methods and events, and the verb each frame and command needs. |
| [CLI and MCP parity](parity.md) | Every MCP tool, the CLI verb it mirrors, how it runs, and its annotations. |

Working on phux? Run `just docs-gen` after changing a command, setting, or anything else listed here; a unit test fails until the pages match the binary.
