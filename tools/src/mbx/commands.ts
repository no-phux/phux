import { parseArgs } from "node:util";
import { Failure, required, UsageError, type Command } from "../lib/command.ts";
import { collapse } from "./collapse.ts";
import { exportHeadroom } from "./headroom.ts";
import { githubStore, prune } from "./prune.ts";

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
  "prune-build-cache": {
    summary: "Retire main's superseded build caches, keeping today's or the newest restore source.",
    usage: "[--dry-run]",
    help: "--dry-run  print the same decisions without deleting (needs GITHUB_REPOSITORY, gh auth)",
    run(args) {
      const { values } = parseArgs({
        args,
        options: { "dry-run": { type: "boolean", default: false } },
      });
      const repository = process.env["GITHUB_REPOSITORY"];
      if (repository === undefined || repository === "")
        throw new Failure("GITHUB_REPOSITORY is required");
      prune(githubStore(repository), new Date(), values["dry-run"]);
      return 0;
    },
  },
};
