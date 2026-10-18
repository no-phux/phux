import type { Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { integrationViolations } from "./versions.ts";

export const commands: Record<string, Command> = {
  "integration-versions-check": {
    summary: "Check agent integration versions, publish settings and runtime ownership.",
    run: () =>
      report(
        integrationViolations(),
        "agent integration versions and neutral runtime ownership are coherent",
      ),
  },
};
