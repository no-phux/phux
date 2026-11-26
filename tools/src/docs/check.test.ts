// Black-box docs gate fixtures: a disposable tree, no compiler or server.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { capture } from "../lib/process.ts";
import { checkDocs, commandCalls, format, type Gate } from "./check.ts";
import { headingAnchors } from "./markdown.ts";

let scratch = "";
let repo = "";

function write(rel: string, text: string): void {
  const path = join(repo, rel);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

function remove(rel: string): void {
  rmSync(join(repo, rel), { recursive: true, force: true });
}

function problems(only?: Gate): string[] {
  return checkDocs(repo, only).violations.map((v) => format(repo, v));
}

function expectPass(only?: Gate): void {
  expect(problems(only)).toEqual([]);
}

function expectFail(expected: string, only?: Gate): void {
  const found = problems(only);
  expect(found.length).toBeGreaterThan(0);
  expect(found.join("\n")).toContain(expected);
}

const GUIDE = `---
group: start
order: 1
---
# Guide
[relative](./Parent/../Parent/Leaf.md#build-cache-mbx)
[root](/docs/Parent/Leaf.md?view=1#repeat-1)
[directory](./Parent/)
[explicit](./Parent/Leaf.md#explicit)
[self](#guide)
[web](https://example.invalid/missing)
\`[inline code](missing.md)\`
\`\`\`md
[code block](missing.md)
\`\`\`
\`\`\`sh
phux agent wait @7
just check
just verify
just scoped
\`\`\`
`;

beforeAll(() => {
  // Resolve /var -> /private/var on macOS before exact component matching.
  scratch = realpathSync(mkdtempSync(join(tmpdir(), "phux-docs-test.")));
  repo = join(scratch, "repo");
  mkdirSync(repo);
  // A Git fixture proves collection ignores untracked build/cache trees
  // normally excluded by Git, but still checks new authored docs.
  capture(["git", "-C", repo, "init", "-q"]);
  write(".gitignore", "ignored/\n");
  write("justfile", "check:\nalias verify := check\n");
  write("just/docs.just", "scoped:\n");
  write(
    "docs/reference/cli.md",
    "# CLI\n\n## `phux agent`\n\n## `phux agent wait`\n\nUsage: phux agent <SUBCOMMAND>\n\n## `phux agent show`\n",
  );
  write(
    "docs/Parent/Leaf.md",
    '# Target\n\n## Build cache (mbx)\n\n## Repeat\n\n## Repeat\n\n<a id="explicit"></a>\n',
  );
});

afterAll(() => rmSync(scratch, { recursive: true, force: true }));

// The cases share one tree and run in order, each rewriting the guide.

test("valid .., root-relative, directory, query, duplicate and explicit anchors pass; code and external links are not followed", () => {
  write("docs/guide.md", GUIDE);
  expectPass();
});

test("an ignored tree is not collected", () => {
  write("ignored/bad.md", "[ignored](missing.md)\n");
  expectPass();
});

test.each([
  ["[wrong parent](./parent/Leaf.md)\n", "parent/Leaf.md does not exist"],
  // Wrong case cannot be hidden by a later .. component.
  ["[wrong parent](./parent/../Parent/Leaf.md)\n", "parent/../Parent/Leaf.md does not exist"],
  ["[wrong leaf](./Parent/leaf.md)\n", "Parent/leaf.md does not exist"],
  ["[missing](./missing.md)\n", "missing.md does not exist"],
  ["[valid](./Parent/Leaf.md#repeat-1)\n[partial](./Parent/Leaf.md#rep)\n", "has no heading #rep"],
  ['[titled](./gone.md "Title")\n', "line 1: ./gone.md does not exist"],
])("links: %j fails", (guide, expected) => {
  write("docs/guide.md", guide);
  expectFail(expected, "links");
});

test("a new untracked doc is still checked", () => {
  write("docs/guide.md", GUIDE);
  write("docs/fresh.md", "[missing](./nowhere.md)\n");
  expectFail("[links] docs/fresh.md: line 1: ./nowhere.md does not exist", "links");
  remove("docs/fresh.md");
});

test("an early anchor on a long page resolves", () => {
  const long = [
    "# Early",
    ...Array.from({ length: 6000 }, (_, i) => `## Heading ${i + 1} with padding`),
  ];
  write("docs/Parent/Long.md", `${long.join("\n")}\n`);
  write("docs/guide.md", "[early](./Parent/Long.md#early)\n");
  expectPass("links");
  remove("docs/Parent/Long.md");
});

test.each([
  ["```sh\nphux gone\n```\n", "`phux gone` is not a command"],
  ["```sh\nphux agent gone\n```\n", "`phux agent gone` is not a command"],
  ["```sh\njust retired\n```\n", "`just retired` is not a recipe"],
  ["Run `just retired` in prose.\n", "`just retired` is not a recipe"],
  ["Run `phux gone --json` in prose.\n", "`phux gone` is not a command"],
])("commands: %j fails", (guide, expected) => {
  write("docs/guide.md", guide);
  expectFail(expected, "commands");
});

test("a text block is not shell", () => {
  write("docs/guide.md", "```text\njust retired\n```\n");
  expectPass("commands");
});

test("a page with no command headings asks for docs-gen", () => {
  const cli = "docs/reference/cli.md";
  write(cli, "# CLI\n");
  expectFail("no command headings; run `just docs-gen`", "commands");
  write(
    cli,
    "# CLI\n\n## `phux agent`\n\n## `phux agent wait`\n\nUsage: phux agent <SUBCOMMAND>\n\n## `phux agent show`\n",
  );
});

test.each([
  ["# No nav\n", "docs/guide.md: no frontmatter group:"],
  ["---\ngroup: nowhere\norder: 1\n---\n# Guide\n", "group nowhere is not one of"],
  ["---\ngroup: help\norder: soon\n---\n# Guide\n", "order: must be a number"],
])("nav: %j fails", (guide, expected) => {
  write("docs/guide.md", guide);
  expectFail(expected, "nav");
});

test.each([
  ["---\ngroup: help\norder: -0.5\n---\n# Guide\n"],
  ["---\npublish: false\n---\n# Internal\n"],
])("nav: %j passes", (guide) => {
  write("docs/guide.md", guide);
  expectPass("nav");
});

test("crates: the table under ## Crates is the inventory", () => {
  write("crates/phux-fixture/Cargo.toml", "[package]\n");
  write("docs/architecture.md", "# Architecture\n");
  expectFail("crates/phux-fixture is not in the crate table", "crates");
  const prose = "\n## Crates\n\nA prose mention of `phux-fixture` is not an inventory row.\n";
  write("docs/architecture.md", `# Architecture\n${prose}`);
  expectFail("crates/phux-fixture is not in the crate table", "crates");
  const table = "\n| Crate | Role |\n|---|---|\n| `phux-fixture` | Fixture |\n";
  write("docs/architecture.md", `# Architecture\n${prose}${table}`);
  expectPass("crates");
  const client = "\nThe standalone client workspace includes `phux-rune`.\n";
  write("docs/architecture.md", `# Architecture\n${prose}${table}${client}`);
  expectPass("crates");
  const stale = "\n| `phux-missing` | Stale root crate |\n";
  write("docs/architecture.md", `# Architecture\n${prose}${table}${client}${stale}`);
  expectFail("phux-missing is not a crate in this tree", "crates");
});

// Code comments under crates/ and clients/ must cite docs that exist;
// strings, URLs, and history permalinks are not citations.
test("citations", () => {
  const lib = "crates/phux-fixture/src/lib.rs";
  const header = [
    "//! Framing (`docs/Parent/Leaf.md#repeat-1`), see also docs/guide.md.",
    "/// [ADR-0007](https://github.com/no-phux/phux/blob/abc/docs/adr/0007-x.md)",
    'const FIXTURE: &str = "docs/missing.md";',
    "",
  ].join("\n");
  write(lib, header);
  write("clients/web/src/wire.h", "/*\n * Wire header (docs/Parent/Leaf.md).\n */\n");
  expectPass("citations");
  write(lib, `${header}// Retired (\`docs/spec/L1.md\` 5.7).\n`);
  expectFail("line 4: docs/spec/L1.md does not exist", "citations");
  write(lib, "//! Header.\n/// Moved (`docs/Parent/Leaf.md#gone`).\n");
  expectFail("docs/Parent/Leaf.md has no heading #gone", "citations");
  write(lib, "/// Wrong case (docs/parent/Leaf.md).\n");
  expectFail("docs/parent/Leaf.md does not exist", "citations");
  write("clients/web/src/tool.py", "    # Retired page, bare (proto.md 4.2).\n");
  write(lib, "//! Header.\n");
  expectFail("proto.md is a retired spec page", "citations");
  remove("clients/web/src/tool.py");
  expectPass("citations");
});

test("outside a Git checkout every *.md is collected", () => {
  const plain = join(scratch, "plain");
  mkdirSync(join(plain, "docs"), { recursive: true });
  writeFileSync(join(plain, "docs/README.md"), "# Docs\n\n[Guide](./guide.md#build-cache-mbx)\n");
  writeFileSync(
    join(plain, "docs/guide.md"),
    "# Guide\n\n## Build cache (mbx)\n\n[gone](./missing.md)\n",
  );
  const found = checkDocs(plain, "links").violations.map((v) => format(plain, v));
  expect(found).toEqual(["[links] docs/guide.md: line 5: ./missing.md does not exist"]);
});

test("heading anchors slug the way GitHub does", () => {
  const page = [
    "# Build cache (mbx)",
    "## [Linked](./x.md) heading ##",
    "## Repeat",
    "## Repeat",
    "```",
    "## fenced",
    "```",
    '<a name="kept"></a>',
  ].join("\n");
  expect(headingAnchors(page, true)).toEqual([
    "build-cache-mbx",
    "linked-heading",
    "repeat",
    "repeat-1",
    "kept",
  ]);
  expect(headingAnchors(page, false)).not.toContain("kept");
});

test("shell blocks drop prompts, comments, wrappers and assignments", () => {
  const page =
    "```console\n$ sudo FOO=1 phux agent wait # just nope\nx | just check; env A=1 phux ls\n```\n";
  expect(commandCalls(page).map((c) => [c.line, c.tool, c.first, c.second])).toEqual([
    [2, "phux", "agent", "wait"],
    [3, "just", "check", ""],
    [3, "phux", "ls", ""],
  ]);
});
