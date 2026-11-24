// The builder index drift check on a fixture tree that mirrors docs/.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { checkLlms, checkLlmsLinks } from "./llms.ts";

let scratch = "";
let repo = "";

function write(rel: string, text: string): void {
  const path = join(repo, rel);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

const ENTRY_POINTS = `- [Versioned \`--json\`](./scripting.md#json-output-and-errors) and the [CLI reference](./reference/cli.md).
- [MCP](./agents.md#mcp) and [parity](./reference/parity.md).
- [\`phux watch\`](./scripting.md#watching-events).
- [Wire protocol](./protocol.md) and [compatibility](./compatibility.md) and [\`wire-schema.json\`](./spec/wire-schema.json).
- [OSC 7501](./program-protocol.md).
- [Plugins](./plugins.md).
- [Agent integrations](./agents.md#agent-setup).
- [Calling phux from a program](./agent-quickstart.md#call-phux-from-a-program) and [embedding](./embedding.md).
`;

const HUB = `# Build on phux

[llms.txt](./llms.txt)

## Entry points

${ENTRY_POINTS}
## Agent state

Agent state is evidence-ranked; screen is the floor.

It has not been measured on a live Claude Code, Codex, OpenCode, or pi session, with or without an integration installed.

## Not shipped

There is no published SDK or client library yet.
[Not shipped](#not-shipped)
There is no public SDK.
There is no JSON socket.
There is no example application.
The static plugin index is built from the GitHub topic \`phux-plugin\`.

[examples](../examples/agents/README.md)
[precedence](./program-sequences.md#precedence)
[plugins manage](./plugins.md#managing-plugins)
[plugins examples](./plugins.md#examples)
[call](./agent-quickstart.md#call-phux-from-a-program)
[run](./agent-quickstart.md#run-without-a-terminal)
[embed](./embedding.md)
[protocol transports](./protocol.md#transports)
[compat change](./compatibility.md#making-a-change-compatible)
[json](./scripting.md#json-output-and-errors)
[npm](./releasing.md#npm-and-cratesio)
[versioning](./releasing.md#versioning)
[watch help](./reference/cli.md#phux-watch)
[send-keys](./reference/cli.md#phux-send-keys)
[paste](./reference/cli.md#phux-paste)
[detach](./reference/cli.md#phux-detach)
[kill](./reference/cli.md#phux-kill)
[attach](./reference/cli.md#phux-attach)
[agent state code](../crates/phux-server/src/agent_state.rs)
[drain](../crates/phux-server/src/runtime/connections/agent_state.rs)
[runtime package](../integrations/runtime/package.json)
`;

const INDEX = `# phux

https://docs.phux.sh/llms.txt

## Entry points

- [Build on phux](./building.md)
${ENTRY_POINTS}
Agent state is evidence-ranked; screen is the floor. It has not been measured on a live Claude Code, Codex, OpenCode, or pi session.
There is no public SDK. There is no JSON socket. There is no example application. The static plugin index is built from the GitHub topic \`phux-plugin\`.
`;

beforeAll(() => {
  scratch = realpathSync(mkdtempSync(join(tmpdir(), "phux-llms-test.")));
  repo = join(scratch, "repo");
  write("docs/scripting.md", "# Scripting\n\n## JSON output and errors\n\n## Watching events\n");
  write("docs/agents.md", "# Agents\n\n## Agent setup\n\n## MCP\n");
  write("docs/reference/parity.md", "# Parity\n");
  // Early matching anchors on a long page still resolve.
  const trailing = Array.from({ length: 10000 }, (_, i) => `## Trailing anchor ${i}`).join("\n");
  write(
    "docs/reference/cli.md",
    "# CLI\n\n## `phux watch`\n# CLI\n\n## `phux send-keys`\n\n## `phux paste`\n\n## `phux detach`\n\n## `phux kill`\n\n## `phux attach`\n" +
      `${trailing}\n`,
  );
  write("docs/protocol.md", "# Protocol\n\n## Transports\n");
  write("docs/compatibility.md", "# Compatibility\n\n## Making a change compatible\n");
  write("docs/spec/wire-schema.json", "{}\n");
  write("docs/program-protocol.md", "# Program protocol\n");
  write("docs/program-sequences.md", "# Sequences\n\n## Precedence\n");
  write("docs/plugins.md", "# Plugins\n\n## Managing plugins\n\n## Examples\n");
  write(
    "docs/agent-quickstart.md",
    "# Quickstart\n\n## Call phux from a program\n\n## Run without a terminal\n",
  );
  write("docs/embedding.md", "# Embed\n");
  write("docs/releasing.md", "# Releasing\n\n## npm and crates.io\n\n## Versioning\n");
  write("crates/phux-server/src/agent_state.rs", "fn main() {}\n");
  write("crates/phux-server/src/runtime/connections/agent_state.rs", "fn drain_state() {}\n");
  write("integrations/runtime/package.json", "{}\n");
  write("examples/agents/README.md", "# Examples\n");
  write("docs/building.md", HUB);
  write("docs/llms.txt", INDEX);
  write("docs/README.md", "[Build on phux](./building.md)\n");
  write("README.md", "[Build on phux](./docs/building.md)\n");
  write("AGENTS.md", "## Building on phux\n\n[Build on phux](./docs/building.md)\n");
});

afterAll(() => rmSync(scratch, { recursive: true, force: true }));

test("the fixture passes", () => {
  expect(checkLlms(repo)).toEqual([]);
});

test.each([
  [
    "a missing anchor",
    "[missing](./reference/cli.md#missing-anchor)\n",
    "has no heading #missing-anchor",
  ],
  // A resolving same-page anchor is [Not shipped](#not-shipped) in the hub.
  [
    "a missing same-page anchor",
    "# Same\n\n## Present\n\n[gone](#not-a-heading)\n",
    "has no heading #not-a-heading",
  ],
  ["a bare #", "[blank](#)\n", "empty link"],
  ["a missing file", "[gone](./gone.txt)\n", "docs/probe.md:1 ./gone.txt does not exist"],
])("%s fails", (_, text, expected) => {
  write("docs/probe.md", text);
  const found = checkLlmsLinks(repo, join(repo, "docs/probe.md"));
  expect(found).toHaveLength(1);
  expect(found[0]).toContain(expected);
});

test("drifted Entry points fail", () => {
  const hub = readFileSync(join(repo, "docs/building.md"), "utf8");
  write(
    "docs/building.md",
    hub.replace("## Entry points\n", "## Entry points\n\n- [only hub](./plugins.md#examples)\n"),
  );
  expect(checkLlms(repo).join("\n")).toContain("Entry points differ");
  write("docs/building.md", hub);
});

test("a missing phrase, a banned phrase, and a missing hub file fail", () => {
  write("docs/llms.txt", INDEX.replace("There is no JSON socket.", ""));
  write(
    "AGENTS.md",
    "## Building on phux\n\n[Build on phux](./docs/building.md)\nIt is seamless.\n",
  );
  const found = checkLlms(repo);
  expect(found).toContain("docs/llms.txt is missing: There is no JSON socket.");
  expect(found).toContain("AGENTS.md has a banned phrase\n  4:It is seamless.");
  write("docs/llms.txt", INDEX);
  write("AGENTS.md", "## Building on phux\n\n[Build on phux](./docs/building.md)\n");
  rmSync(join(repo, "docs/building.md"));
  expect(checkLlms(repo)).toEqual([`missing ${repo}/docs/building.md`]);
  write("docs/building.md", HUB);
});
