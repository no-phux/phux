import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { bootstrap, exportSeries, layout, readPin } from "./source.ts";
import { testWgpu } from "./wgpu.ts";

const SOURCE_HELP = `  (none)    materialize (or verify) clients/desktop/toolchain/gpui
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
    usage: "[--check | --edit | --export]",
    help: SOURCE_HELP,
    async run(args) {
      const flag = { type: "boolean" } as const;
      const { values } = parseArgs({ args, options: { check: flag, edit: flag, export: flag } });
      const [mode, ...extra] = Object.keys(values);
      if (extra.length > 0) throw new UsageError("choose one of --check, --edit and --export");
      const paths = layout();
      if (mode === "export") {
        const count = exportSeries(readPin(paths), paths);
        console.log(`exported ${count} patch(es); review patches/ and source.json`);
      } else {
        console.log(
          await bootstrap(mode === "check" || mode === "edit" ? mode : "materialize", paths),
        );
      }
      return 0;
    },
  },
  "desktop-wgpu-test": {
    summary:
      "Test the pinned wgpu renderer crate in a disposable workspace (args go to cargo test).",
    usage: "[CARGO_TEST_ARGS...]",
    async run(args) {
      const paths = layout();
      console.log(await bootstrap("materialize", paths));
      return testWgpu(args, paths);
    },
  },
};
