---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# tools

**TL;DR.** phux's build and CI logic in one TypeScript package on the pinned
Bun ([ADR-0183](../docs/adr/0183-build-and-ci-logic-is-one-bun-package.md)).
Run `bun tools/main.ts` for the command list, `just tools-check` to
format-check, typecheck, lint and test it.

## Layout

- `main.ts` lists every command; each area exports its own from
  `src/<area>/commands.ts`.
- `src/lib/` holds what more than one area uses: the Cargo path graph
  (`cargo.ts`), git, GitHub Actions outputs, the GitHub API, value narrowing
  for parsed TOML and JSON, and human diagnostics (`output.ts`).
- `src/<area>/*.test.ts` sit next to the code they test and run with
  `bun test`. Tests call functions directly; a fixture git repository or a
  fake `gh` on `PATH` stands in for the outside world, never a module mock.

## Rules

- Runtime code imports only Bun and `node:` built-ins. `devDependencies`
  are the typecheck, lint and format tools; nothing needs `bun install` to
  run a command.
- Facts Cargo already holds (crate names, path dependencies, versions,
  features) are read from the manifests, never copied into a table.
- Workflows and recipes call `bun tools/main.ts <command>`; a CI job gets Bun
  from `.github/actions/setup-bun`.
- Shell stays shell: a recipe or workflow step that only sequences other
  programs is not moved here.
