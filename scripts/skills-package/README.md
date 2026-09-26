# phux agent skills

[![skills.sh](https://skills.sh/b/no-phux/skills)](https://skills.sh/no-phux/skills)

Agent skills for driving persistent terminals with phux.

```sh
npx skills add no-phux/skills
```

Install one skill:

```sh
npx skills add no-phux/skills --skill using-phux
npx skills add no-phux/skills --skill using-phux-mcp
```

From the well-known index on phux.sh:

```sh
npx skills add https://phux.sh
```

## Skills

**using-phux.** Drives persistent terminals and supervises terminal-hosted
agents with the phux CLI. Use for REPLs, debuggers, dev servers, interactive
programs, durable shell state, or multi-agent terminal workflows; prefer a
one-shot shell for independent commands.

**using-phux-mcp.** Drives persistent terminals and terminal-hosted agents
through the phux MCP server. Use when phux MCP tools are available for
interactive programs, durable shell state, bounded observation, or agent
supervision; use using-phux for direct CLI work.

## Source of truth

This repository is a publish mirror of
[no-phux/phux](https://github.com/no-phux/phux) `.agents/skills/`. Edit skills
there; pushes to `main` refresh this tree. Do not open PRs against this
mirror.

`phux --skill` and `phux mcp --skill` remain the version-matched copies
compiled into the installed binaries.

## License

[Apache-2.0](./LICENSE). Copyright 2026 phall.
