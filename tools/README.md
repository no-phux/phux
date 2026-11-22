# tools

phux's build and CI logic in one TypeScript package on the pinned Bun. Run
`bun tools/main.ts` for the command list, `just tools-check` to
format-check, typecheck, lint and test it.

## Layout

- `main.ts` lists every command; each area exports its own from
  `src/<area>/commands.ts`.
- `src/lib/` holds what more than one area uses: the command contract
  (`command.ts`), child processes (`process.ts`), git and `gh`, the Cargo
  path graph (`cargo.ts`), GitHub Actions outputs, value narrowing for parsed
  TOML and JSON, and human diagnostics (`output.ts`).
- `src/<area>/*.test.ts` sit next to the code they test and run with
  `bun test`. Tests call functions directly; a fixture git repository or a
  fake `gh` on `PATH` stands in for the outside world, never a module mock.

## Rules

- A command declares `summary` and, if it takes arguments, `usage`; it
  throws `UsageError` for a bad command line (exit 2, usage printed) and
  `Failure` for an expected failure (exit 1, message, no stack). `main.ts`
  owns `--help`, and refuses arguments to a command with no `usage`.

- Runtime code imports only Bun and `node:` built-ins. `devDependencies`
  are the typecheck, lint and format tools; nothing needs `bun install` to
  run a command.
- Facts Cargo already holds (crate names, path dependencies, versions,
  features) are read from the manifests, never copied into a table.
- Workflows and recipes call `bun tools/main.ts <command>`; a CI job gets Bun
  from `.github/actions/setup-bun`.
- Shell stays shell: a recipe or workflow step that only sequences other
  programs is not moved here.
