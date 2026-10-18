import { parseArgs } from "node:util";
import { required, UsageError, type Command } from "../lib/command.ts";
import { writeSection } from "./changelog-section.ts";
import { main as dispatchIntegrations } from "./dispatch-integrations.ts";
import { main as nextPlan } from "./next-plan.ts";
import { main as publishPlan } from "./publish-plan.ts";
import { releaseDrift } from "./release-drift.ts";
import { releaseManifest } from "./release-manifest.ts";

const text = { type: "string" } as const;

export const commands: Record<string, Command> = {
  "publish-plan": {
    summary:
      "Choose the drafts release.yml ships and delete superseded ones (env in, outputs out).",
    run: () => publishPlan(),
  },
  "release-manifest": releaseManifest,
  "next-plan": {
    summary: "Decide which products next.yml rebuilds for a green main SHA (outputs).",
    usage: "SHA",
    run(args) {
      const [sha, ...extra] = parseArgs({ args, allowPositionals: true }).positionals;
      if (sha === undefined || extra.length > 0) throw new UsageError("expected one SHA");
      return nextPlan(sha);
    },
  },
  "dispatch-integration-publishes": {
    summary: "Dispatch agent-integration-release.yml for each of TAGS and wait for it.",
    run: () => dispatchIntegrations(),
  },
  "changelog-section": {
    summary: "Print or write one release tag's Keep-a-Changelog section, as of the tag.",
    usage: "--tag TAG [--changelog PATH] [--ref REF] [--output PATH]",
    help: "PATH defaults to CHANGELOG.md and REF to refs/tags/TAG.",
    run(args) {
      const options = { tag: text, changelog: text, ref: text, output: text };
      const { values } = parseArgs({ args, options });
      const tag = required(values.tag, "--tag");
      const ref = values.ref ?? `refs/tags/${tag}`;
      return writeSection(tag, values.changelog ?? "CHANGELOG.md", ref, values.output);
    },
  },
  "release-drift": {
    summary: "Report stuck releases: old drafts, assetless publishes, untagged release PRs.",
    usage: "[--grace-minutes N]",
    run(args) {
      const options = { "grace-minutes": { ...text, default: "120" } };
      const grace = Number(parseArgs({ args, options }).values["grace-minutes"]);
      if (!(grace >= 0)) throw new UsageError("--grace-minutes must be a number >= 0");
      return releaseDrift(grace);
    },
  },
};
