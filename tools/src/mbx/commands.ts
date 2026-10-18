import { required, UsageError, type Command } from "../lib/command.ts";
import { collapse } from "./collapse.ts";

export const commands: Record<string, Command> = {
  "mbx-collapse-target-copies": {
    summary: "Share duplicate mbx target bytes with the object store.",
    usage: "CACHE_DIR",
    run([cacheDir, ...rest]) {
      if (rest.length > 0) throw new UsageError(`unexpected ${rest.join(" ")}`);
      collapse(required(cacheDir, "CACHE_DIR"));
      return 0;
    },
  },
};
