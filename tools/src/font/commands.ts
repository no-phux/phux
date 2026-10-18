import { readFileSync, writeFileSync } from "node:fs";
import { describe, Failure, UsageError, type Command } from "../lib/command.ts";
import { hint, status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { SOURCE, TABLE, emit, parseBdf } from "./bitmap.ts";

function read(path: string): string {
  try {
    return readFileSync(repoPath(path), "utf8");
  } catch (error) {
    throw new Failure(`cannot read ${path}: ${describe(error)}`);
  }
}

/** The first line where `committed` and `fresh` differ, or undefined when equal. */
export function firstDifference(committed: string, fresh: string): string | undefined {
  if (committed === fresh) return undefined;
  const [left, right] = [committed.split("\n"), fresh.split("\n")];
  const index = left.findIndex((line, at) => line !== right[at]);
  const at = index === -1 ? left.length : index;
  return `line ${at + 1}:\n- ${left[at] ?? "(end of file)"}\n+ ${right[at] ?? "(end of file)"}`;
}

export const commands: Record<string, Command> = {
  "gen-bitmap-font": {
    summary: "Regenerate the Spleen glyph table from its BDF face, or --check it.",
    usage: "[--check]",
    help: `Reads ${SOURCE} and writes ${TABLE}.`,
    run(args) {
      const check = args[0] === "--check";
      if (args.length > (check ? 1 : 0)) throw new UsageError(`unexpected ${args.join(" ")}`);
      const fresh = emit(parseBdf(read(SOURCE)));
      if (!check) {
        writeFileSync(repoPath(TABLE), fresh);
        console.log(`wrote ${TABLE}`);
        return 0;
      }
      const difference = firstDifference(read(TABLE), fresh);
      if (difference === undefined) {
        status("ok", `${TABLE} matches ${SOURCE}`);
        return 0;
      }
      status(
        "error",
        `${TABLE} is not a fresh regeneration of ${SOURCE}; it is generated, do not edit it`,
      );
      hint(`first difference (committed -, regenerated +) at ${difference}`);
      hint("fix with: bun tools/main.ts gen-bitmap-font, then commit the table with the .bdf");
      return 1;
    },
  },
};
