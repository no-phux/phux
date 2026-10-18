import { UsageError, type Command } from "../lib/command.ts";
import { collapse } from "./collapse.ts";

export const commands: Record<string, Command> = {
  "mbx-collapse-target-copies": {
    summary: "Share duplicate mbx target bytes with the object store.",
    usage: "CACHE_DIR",
    run(args) {
      const [cacheDir] = args;
      if (args.length !== 1 || cacheDir === undefined) throw new UsageError("expected CACHE_DIR");
      collapse(cacheDir);
      return 0;
    },
  },
};
