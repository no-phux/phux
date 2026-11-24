// Every remote action is pinned by full commit SHA with a trailing `# v<tag>`
// comment. Tags are mutable: a retagged release swaps the code every workflow
// runs with no diff here. The SHA is immutable, and the comment keeps the pin
// human-auditable and lets Dependabot propose bumps. Local actions
// (`./.github/...`) and `docker://` images are exempt: the first is this tree,
// reviewed in the same diff.
//
// Pin format: uses: owner/repo@<40-hex-sha> # v<tag>. Resolve a tag with
//   git ls-remote https://github.com/<owner>/<repo> "refs/tags/<tag>^{}"
import { expect, test } from "bun:test";
import { actionNames, actionText, workflowNames, workflowText } from "./workflows.ts";

const USES = /^\s*-?\s*uses: (\S+)/;

/** One line per `uses:` that breaks the pin policy, labelled `file:line`. */
function pinViolations(file: string, text: string): string[] {
  return text.split("\n").flatMap((line, index) => {
    const ref = USES.exec(line)?.[1];
    if (ref === undefined || ref.startsWith("./") || ref.startsWith("docker://")) return [];
    const where = `${file}:${index + 1}`;
    if (!/^[0-9a-f]{40}$/.test(ref.slice(ref.lastIndexOf("@") + 1))) {
      return [`${where}: action is not SHA-pinned: ${ref}`];
    }
    if (!line.includes("# v"))
      return [`${where}: SHA pin missing trailing "# v<tag>" comment: ${ref}`];
    return [];
  });
}

const SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1";

test("the pin policy accepts SHA pins with a tag comment and local actions", () => {
  const text = [
    `      - uses: actions/checkout@${SHA} # v7.0.1`,
    `        uses: actions/cache/save@${SHA} # v5`,
    "      - uses: ./.github/actions/setup-bun",
    "      - uses: docker://alpine:3",
    "        run: echo uses: nothing",
  ].join("\n");
  expect(pinViolations("w.yml", text)).toEqual([]);
});

test("the pin policy rejects tags, short SHAs and a missing tag comment", () => {
  const text = [
    "      - uses: actions/checkout@v7",
    `      - uses: actions/checkout@${SHA.slice(0, 12)} # v7`,
    `      - uses: actions/checkout@${SHA}`,
    `      - uses: actions/checkout@${SHA} # 7.0.1`,
  ].join("\n");
  expect(pinViolations("w.yml", text)).toEqual([
    "w.yml:1: action is not SHA-pinned: actions/checkout@v7",
    `w.yml:2: action is not SHA-pinned: actions/checkout@${SHA.slice(0, 12)}`,
    `w.yml:3: SHA pin missing trailing "# v<tag>" comment: actions/checkout@${SHA}`,
    `w.yml:4: SHA pin missing trailing "# v<tag>" comment: actions/checkout@${SHA}`,
  ]);
});

test("every workflow and composite action pins its remote actions", () => {
  const violations = [
    ...workflowNames().flatMap((name) =>
      pinViolations(`.github/workflows/${name}`, workflowText(name)),
    ),
    ...actionNames().flatMap((name) =>
      pinViolations(`.github/actions/${name}/action.yml`, actionText(name)),
    ),
  ];
  expect(violations).toEqual([]);
});
