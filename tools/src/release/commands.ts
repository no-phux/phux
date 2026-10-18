import { parseArgs } from "node:util";
import type { Command } from "../lib/command.ts";
import { writeSection } from "./changelog-section.ts";
import { main as dispatchIntegrations } from "./dispatch-integrations.ts";
import { milestoneLabels } from "./milestone-labels.ts";
import { main as publishPlan } from "./publish-plan.ts";
import { releaseDrift } from "./release-drift.ts";
import { execute, isProduct, USAGE, type Invocation } from "./release-manifest.ts";
import { run as validationReceipt } from "./validation-receipt.ts";

class UsageError extends Error {}

/** argparse's contract: a usage error prints the usage and exits 2. */
function withUsage(usage: string, body: () => number): number {
  try {
    return body();
  } catch (error) {
    const parse = error instanceof TypeError && "code" in error;
    if (!(error instanceof UsageError) && !parse) throw error;
    console.error(`${usage}\n\nerror: ${error.message}`);
    return 2;
  }
}

function product(value: string | undefined): "phux" | "desktop" {
  if (value === undefined || !isProduct(value)) {
    throw new UsageError(`product must be phux or desktop, got ${String(value)}`);
  }
  return value;
}

function required(value: string | undefined, flag: string): string {
  if (value === undefined) throw new UsageError(`${flag} is required`);
  return value;
}

export function parseManifestArgs(args: string[]): Invocation {
  const [command, ...rest] = args;
  switch (command) {
    case "stable": {
      const { values } = parseArgs({
        args: rest,
        options: { out: { type: "string" }, publish: { type: "boolean", default: false } },
      });
      return { command, out: required(values.out, "--out"), publish: values.publish };
    }
    case "next": {
      const { values } = parseArgs({
        args: rest,
        options: {
          product: { type: "string" },
          sha: { type: "string" },
          dist: { type: "string" },
        },
      });
      return {
        command,
        product: product(required(values.product, "--product")),
        sha: required(values.sha, "--sha"),
        dist: required(values.dist, "--dist"),
      };
    }
    case "next-sha": {
      const { positionals } = parseArgs({ args: rest, allowPositionals: true });
      if (positionals.length !== 1) throw new UsageError("next-sha takes one PRODUCT");
      return { command, product: product(positionals[0]) };
    }
    case "fixtures": {
      const { values } = parseArgs({
        args: rest,
        options: { check: { type: "boolean", default: false } },
      });
      return { command, check: values.check };
    }
    default:
      throw new UsageError(`unknown command ${String(command)}`);
  }
}

const CHANGELOG_USAGE =
  "usage: bun tools/main.ts changelog-section --tag TAG --changelog PATH [--output PATH]";
const RECEIPT_USAGE = "usage: bun tools/main.ts validation-receipt {check,write} WORKFLOW";

export const commands: Record<string, Command> = {
  "publish-plan": {
    summary:
      "Choose the drafts publish.yml ships and delete superseded ones (env in, outputs out).",
    run: () => publishPlan(),
  },
  "release-manifest": {
    summary: "Build or publish the stable/next channel manifests, or check their fixtures.",
    run: (args) => withUsage(USAGE, () => execute(parseManifestArgs(args))),
  },
  "dispatch-integration-publishes": {
    summary: "Dispatch agent-integration-release.yml for each of TAGS and wait for it.",
    run: () => dispatchIntegrations(),
  },
  "changelog-section": {
    summary: "Print or write one release tag's Keep-a-Changelog section.",
    run: (args) =>
      withUsage(CHANGELOG_USAGE, () => {
        const { values } = parseArgs({
          args,
          options: {
            tag: { type: "string" },
            changelog: { type: "string" },
            output: { type: "string" },
          },
        });
        const [tag, changelog] = [
          required(values.tag, "--tag"),
          required(values.changelog, "--changelog"),
        ];
        return writeSection(tag, changelog, values.output);
      }),
  },
  "validation-receipt": {
    summary: "Write, or on a push find, a receipt of validation for this exact tree and workflow.",
    run: (args) =>
      withUsage(RECEIPT_USAGE, () => {
        const { positionals } = parseArgs({ args, allowPositionals: true });
        const [mode, workflow] = positionals;
        if (positionals.length !== 2 || (mode !== "check" && mode !== "write")) {
          throw new UsageError("expected a mode (check or write) and a workflow path");
        }
        validationReceipt(mode, required(workflow, "WORKFLOW"));
        return 0;
      }),
  },
  "release-drift": {
    summary: "Report stuck releases: old drafts, assetless publishes, untagged release PRs.",
    run: () => releaseDrift(),
  },
  "milestone-labels": {
    summary: "Check every non-closed bead carries exactly one of rc-1.0 / post-1.0 (local).",
    run: () => milestoneLabels(),
  },
};
