import { parseArgs } from "node:util";
import type { Command } from "../lib/command.ts";
import { GitError } from "../lib/git.ts";
import { status } from "../lib/output.ts";
import { bootstrap, exportSeries, layout, readPin, SourceError } from "./source.ts";
import { testWgpu } from "./wgpu.ts";

const USAGE = `usage: bun tools/main.ts desktop-source [--check | --edit | --export]

  (none)    materialize (or verify) clients/desktop/toolchain/gpui
  --check   verify only; fail when missing
  --edit    materialize as a git repo: one commit per patch
  --export  write that repo's commits back as patches/ + pins

Changing the framework: --edit, change code under toolchain/gpui, commit
(amend the matching patch commit, or add a new commit for a new patch), then
--export. The commit subject names the patch file. --export rewrites
patches/ and source.json to exactly the repo's series, so review the diff.`;

export const commands: Record<string, Command> = {
  "desktop-source": {
    summary: "Materialize or verify the pinned, patched GPUI crates the desktop builds against.",
    async run(args) {
      let values;
      try {
        ({ values } = parseArgs({
          args,
          options: {
            check: { type: "boolean" },
            edit: { type: "boolean" },
            export: { type: "boolean" },
          },
        }));
      } catch {
        console.error(USAGE);
        return 2;
      }
      const chosen = [values.check, values.edit, values.export].filter(Boolean).length;
      if (chosen > 1) {
        console.error(USAGE);
        return 2;
      }
      const paths = layout();
      try {
        if (values.export) {
          const count = exportSeries(readPin(paths), paths);
          console.log(`exported ${count} patch(es); review patches/ and source.json`);
          return 0;
        }
        const mode = values.check ? "check" : values.edit ? "edit" : "materialize";
        console.log(await bootstrap(mode, paths));
        return 0;
      } catch (error) {
        if (!(error instanceof SourceError || error instanceof GitError)) throw error;
        status("error", error.message);
        return 1;
      }
    },
  },
  "desktop-wgpu-test": {
    summary:
      "Test the pinned wgpu renderer crate in a disposable workspace (args go to cargo test).",
    async run(args) {
      const paths = layout();
      console.log(await bootstrap("materialize", paths));
      return testWgpu(args, paths);
    },
  },
};
