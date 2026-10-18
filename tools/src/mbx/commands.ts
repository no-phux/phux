import type { Command } from "../lib/command.ts";
import { collapse } from "./collapse.ts";

export const commands: Record<string, Command> = {
  "mbx-collapse-target-copies": {
    summary: "Share duplicate mbx target bytes with the object store: <CACHE_DIR>.",
    run(args) {
      const [cacheDir] = args;
      if (args.length !== 1 || cacheDir === undefined) {
        console.error("usage: bun tools/main.ts mbx-collapse-target-copies CACHE_DIR");
        return 2;
      }
      collapse(cacheDir);
      return 0;
    },
  },
};
