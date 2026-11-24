import { required, UsageError, type Command } from "../lib/command.ts";
import { collapse } from "./collapse.ts";
import { exportHeadroom } from "./headroom.ts";

export const commands: Record<string, Command> = {
  "mbx-export-headroom": {
    summary: "Leave disk for the mbx export post step (CI; PHUX_MBX_EXPORT_HEADROOM=1 locally).",
    run() {
      exportHeadroom();
      return 0;
    },
  },
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
