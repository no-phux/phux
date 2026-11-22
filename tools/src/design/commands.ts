import { resolve } from "node:path";
import { parseArgs } from "node:util";
import { Failure, type Command } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { committed, manifest, mobile, renderTokens, writeExports } from "./exports.ts";
import { SOURCES, loadSources } from "./sources.ts";
import { suggest } from "./suggest.ts";

async function designGen(args: string[]): Promise<number> {
  const { values } = parseArgs({
    args,
    options: {
      check: { type: "boolean", default: false },
      suggest: { type: "boolean", default: false },
      "mobile-out": { type: "string" },
    },
  });
  if (values.suggest) {
    console.log(JSON.stringify(suggest(await loadSources(ROOT)), null, 1));
    return 0;
  }
  const { exports } = await renderTokens(ROOT);
  const stale = await writeExports(ROOT, committed(exports), values.check);
  if (stale.length > 0) {
    throw new Failure(`stale design exports (run just design-gen):\n  ${stale.join("\n  ")}`);
  }
  const out = values["mobile-out"];
  if (out !== undefined) {
    const files = mobile(exports);
    const bytes = manifest(files, git(["rev-parse", "HEAD"]).trim());
    await writeExports(resolve(out), [...files, { path: "manifest.json", bytes }]);
  }
  console.log(`design exports ${values.check ? "verified" : "written"} from ${SOURCES}`);
  return 0;
}

export const commands: Record<string, Command> = {
  "design-gen": {
    summary: "Generate the design-token exports from crates/phux-design/tokens/src.",
    usage: "[--check] [--suggest] [--mobile-out DIR]",
    help: [
      "--check           refuse committed exports that differ from a fresh render",
      "--suggest         print the least-move fixes for failing contrast floors (writes nothing)",
      "--mobile-out DIR  also write the mobile exports and manifest.json into DIR",
    ].join("\n"),
    run: designGen,
  },
};
