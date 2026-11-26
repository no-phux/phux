// Every remote action is referenced by full commit SHA with a trailing
// `# v<tag>` comment. Tags are mutable: a retagged release swaps the code every
// workflow runs with no diff here. The SHA is immutable, and the version
// comment keeps the pin auditable (and lets Dependabot propose bumps). Local
// reusable workflows and actions (`uses: ./...`) are this tree, reviewed in the
// same diff; `docker://` images are out of scope.
import { expect, test } from "bun:test";
import { actionNames, actionText, workflowNames, workflowText } from "./workflows.ts";

const USES = /^\s*-?\s*uses: (\S+)/;
const SHA = /^[0-9a-f]{40}$/;
const HINT =
  'pin as uses: owner/repo@<40-hex-sha> # v<tag>; resolve with git ls-remote https://github.com/<owner>/<repo> "refs/tags/<tag>^{}"';

function pinFailures(file: string, text: string): string[] {
  return text.split("\n").flatMap((line, index) => {
    const ref = USES.exec(line)?.[1];
    if (ref === undefined || ref.startsWith("./") || ref.startsWith("docker://")) return [];
    const where = `${file}:${index + 1}`;
    if (!SHA.test(ref.slice(ref.lastIndexOf("@") + 1))) {
      return [`${where}: action is not SHA-pinned: ${ref} (${HINT})`];
    }
    if (!line.includes("# v")) {
      return [`${where}: SHA pin missing trailing "# v<tag>" comment: ${ref} (${HINT})`];
    }
    return [];
  });
}

test("every workflow and composite action pins remote actions by SHA with a version comment", () => {
  const files: [string, string][] = [
    ...workflowNames().map((name): [string, string] => [
      `.github/workflows/${name}`,
      workflowText(name),
    ]),
    ...actionNames().map((name): [string, string] => [
      `.github/actions/${name}/action.yml`,
      actionText(name),
    ]),
  ];
  expect(files.length).toBeGreaterThan(0);
  expect(files.flatMap(([file, text]) => pinFailures(file, text))).toEqual([]);
});

test("a tag, a branch, a short SHA, or a missing version comment is refused", () => {
  const sha = "3d3c42e5aac5ba805825da76410c181273ba90b1";
  const text = [
    "steps:",
    `  - uses: actions/checkout@${sha} # v7.0.1`,
    "  - uses: ./.github/actions/setup-bun",
    "    uses: ./.github/workflows/build-cli.yml",
    "  - uses: docker://alpine:3",
    "  - uses: actions/checkout@v7",
    "  - uses: actions/checkout@main",
    `  - uses: actions/checkout@${sha.slice(0, 12)} # v7.0.1`,
    `  - uses: actions/checkout@${sha}`,
    `    uses: actions/checkout@${sha} # pinned`,
  ].join("\n");
  expect(pinFailures("x.yml", text).map((failure) => failure.split(" (")[0])).toEqual([
    "x.yml:6: action is not SHA-pinned: actions/checkout@v7",
    "x.yml:7: action is not SHA-pinned: actions/checkout@main",
    `x.yml:8: action is not SHA-pinned: actions/checkout@${sha.slice(0, 12)}`,
    `x.yml:9: SHA pin missing trailing "# v<tag>" comment: actions/checkout@${sha}`,
    `x.yml:10: SHA pin missing trailing "# v<tag>" comment: actions/checkout@${sha}`,
  ]);
});
