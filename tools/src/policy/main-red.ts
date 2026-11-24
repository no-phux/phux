// Report main's health after the merge (.github/workflows/main-red.yml).
//
// Pull requests merge on a fast Linux gate; the slow lanes (native setup, rust
// icount, Rune Linux qualification, Apple) first run on main's push. When one
// of those push runs ends red, this opens the one `ci/main-red` issue (later
// reds comment on it) and lists the commits since that workflow's last green
// main push as suspects. When every tracked workflow's latest main push run is
// green again, it closes the issue.
//
// Reads only run metadata and commit subjects; never checks out or runs the
// reported run's code. Inputs arrive through the environment, never through
// workflow expression interpolation.
import { Failure } from "../lib/command.ts";
import { api, gh, type Api } from "../lib/github.ts";
import { list, str, table } from "../lib/value.ts";

const LABEL = "ci/main-red";
const INPUTS = [
  "GH_REPO",
  "WORKFLOW_NAME",
  "RUN_URL",
  "RUN_CREATED_AT",
  "CONCLUSION",
  "HEAD_SHA",
] as const;
type Input = (typeof INPUTS)[number];

interface Run {
  name: string;
  createdAt: string;
  headSha: string;
  conclusion: string;
}

class MainRed {
  readonly tracked: string[];

  constructor(
    private readonly input: Record<Input, string>,
    tracked: string,
    private readonly rest: Api,
  ) {
    this.tracked = tracked.split(/\s+/).filter((name) => name !== "");
  }

  private runs(status: "success" | "completed"): Run[] {
    const path = `repos/${this.input.GH_REPO}/actions/runs?branch=main&event=push&status=${status}&per_page=50`;
    return list(table(this.rest.json(path))["workflow_runs"]).map((value) => {
      const run = table(value);
      return {
        name: str(run["name"]) ?? "",
        createdAt: str(run["created_at"]) ?? "",
        headSha: str(run["head_sha"]) ?? "",
        conclusion: str(run["conclusion"]) ?? "",
      };
    });
  }

  private openIssue(): string | undefined {
    const number = gh([
      "issue",
      "list",
      "--label",
      LABEL,
      "--state",
      "open",
      "--limit",
      "1",
      "--json",
      "number",
      "--jq",
      ".[0].number // empty",
    ]).trim();
    return number === "" ? undefined : number;
  }

  /** The newest green main push run of this workflow created before this run. */
  private lastGreenSha(): string | undefined {
    const { WORKFLOW_NAME: name, RUN_CREATED_AT: before } = this.input;
    return (
      this.runs("success").find((run) => run.name === name && run.createdAt < before)?.headSha ||
      undefined
    );
  }

  private suspects(): string {
    const { GH_REPO: repo, HEAD_SHA: head, WORKFLOW_NAME: name } = this.input;
    const line = (commit: unknown): string => {
      const sha = str(table(commit)["sha"]) ?? "";
      const message = str(table(table(commit)["commit"])["message"]) ?? "";
      return `- ${sha.slice(0, 10)} ${message.split("\n")[0]}`;
    };
    const good = this.lastGreenSha();
    if (good === undefined) return line(this.rest.json(`repos/${repo}/commits/${head}`));
    const commits = list(
      table(this.rest.json(`repos/${repo}/compare/${good}...${head}`))["commits"],
    );
    return `${commits.toReversed().map(line).join("\n")}\n\nLast green \`${name}\` push: \`${good.slice(0, 10)}\`.`;
  }

  /** Whether the latest completed main push run of every tracked workflow passed. */
  private allGreen(): boolean {
    const runs = this.runs("completed");
    return this.tracked.every((name) => {
      const latest = runs.find((run) => run.name === name && run.conclusion !== "cancelled");
      return (latest?.conclusion ?? "success") === "success";
    });
  }

  red(): void {
    const { WORKFLOW_NAME: name, HEAD_SHA: head, RUN_URL: url } = this.input;
    const issue = this.openIssue();
    const body = `\`${name}\` failed on main at \`${head.slice(0, 10)}\`: ${url}\n\nSuspect commits, oldest first:\n\n${this.suspects()}\n`;
    if (issue !== undefined) {
      gh(["issue", "comment", issue, "--body", body]);
      return;
    }
    try {
      gh([
        "label",
        "create",
        LABEL,
        "--color",
        "B60205",
        "--description",
        "main's post-merge CI is red",
      ]);
    } catch {
      // The label already exists.
    }
    gh([
      "issue",
      "create",
      "--label",
      LABEL,
      "--title",
      `main is red: ${name} at ${head.slice(0, 10)}`,
      "--body",
      body,
    ]);
  }

  green(): void {
    const issue = this.openIssue();
    if (issue === undefined || !this.allGreen()) return;
    const comment = `Green again at \`${this.input.HEAD_SHA.slice(0, 10)}\`: every tracked workflow (${this.tracked.join(" ")}) passed its latest main push.`;
    gh(["issue", "close", issue, "--comment", comment]);
  }
}

/** Open, update or close the ci/main-red issue for one completed main push run. */
export function mainRed(
  env: Record<string, string | undefined> = process.env,
  rest: Api = api,
): number {
  const missing = INPUTS.filter((name) => !env[name]);
  if (missing.length > 0) throw new Failure(`main-red: missing ${missing.join(", ")}`);
  const input = Object.fromEntries(INPUTS.map((name) => [name, env[name] ?? ""])) as Record<
    Input,
    string
  >;
  const report = new MainRed(input, env["TRACKED_WORKFLOWS"] || "ci rune-macos", rest);
  switch (input.CONCLUSION) {
    case "failure":
    case "timed_out":
      report.red();
      break;
    case "success":
      report.green();
      break;
    default:
      console.log(`main-red: ignoring conclusion ${input.CONCLUSION}`);
  }
  return 0;
}
