import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { formatOutputs } from "../lib/actions.ts";
import type { Command } from "../lib/command.ts";
import { Classifier, toOutputs, type Classification } from "./classify.ts";
import { detect, diffPaths } from "./detect.ts";
import { SURFACES } from "./routes.ts";

/** Outputs for a release-please metadata diff: no compile lane. */
export const RELEASE_METADATA: Classification = {
  // SAFETY: built from SURFACES, so every Surface key is present.
  needed: Object.fromEntries(
    SURFACES.map((surface) => [surface, false]),
  ) as Classification["needed"],
  docsOnly: false,
  perfIcountNeeded: false,
  testFilterset: "",
  unitMode: "skip",
  unitTargets: "",
  e2eNeeded: false,
};

export const commands: Record<string, Command> = {
  classify: {
    summary: "Route changed paths (stdin, or --since REV [--until REV]) to CI lanes.",
    run(args) {
      const { values } = parseArgs({
        args,
        options: { since: { type: "string" }, until: { type: "string", default: "HEAD" } },
      });
      const paths =
        values.since === undefined
          ? readFileSync(0, "utf8").split("\n").filter(Boolean)
          : diffPaths(values.since, values.until);
      console.log(formatOutputs(toOutputs(new Classifier().classify(paths))));
      return 0;
    },
  },
  "detect-changes": {
    summary: "Route the current GitHub Actions event (GITHUB_EVENT_NAME/PATH) to CI lanes.",
    run() {
      const detection = detect(
        process.env["GITHUB_EVENT_NAME"] ?? "",
        process.env["GITHUB_EVENT_PATH"],
      );
      let result: Classification;
      let releaseMetadataOnly = false;
      switch (detection.kind) {
        case "release-metadata":
          console.error("Release metadata only; compile lanes skipped");
          result = RELEASE_METADATA;
          releaseMetadataOnly = true;
          break;
        case "full":
          console.error(`Full validation: ${detection.reason}`);
          result = new Classifier().classify([]);
          break;
        case "paths":
          result = new Classifier().classify(detection.paths);
      }
      console.log(
        formatOutputs({ ...toOutputs(result), release_metadata_only: releaseMetadataOnly }),
      );
      return 0;
    },
  },
};
