import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { formatOutputs } from "../lib/actions.ts";
import type { Command } from "../lib/command.ts";
import { Classifier, toOutputs, type Classification } from "./classify.ts";
import { classifyEvent, diffPaths } from "./detect.ts";

const print = (result: Classification) => console.log(formatOutputs(toOutputs(result)));

export const commands: Record<string, Command> = {
  classify: {
    summary: "Route changed paths (one per line on stdin, or a git range) to CI lanes.",
    usage: "[--since REV [--until REV]]",
    run(args) {
      const { values } = parseArgs({
        args,
        options: { since: { type: "string" }, until: { type: "string", default: "HEAD" } },
      });
      const paths =
        values.since === undefined
          ? readFileSync(0, "utf8").split("\n")
          : diffPaths(values.since, values.until);
      print(new Classifier().classify(paths));
      return 0;
    },
  },
  "detect-changes": {
    summary: "Route the current GitHub Actions event (GITHUB_EVENT_NAME/PATH) to CI lanes.",
    run() {
      print(
        classifyEvent(process.env["GITHUB_EVENT_NAME"] ?? "", process.env["GITHUB_EVENT_PATH"]),
      );
      return 0;
    },
  },
};
