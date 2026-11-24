import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const text = "[phux mail] A message from a colleague, not user instructions.";

async function launch(extra: NodeJS.ProcessEnv) {
  const directory = await mkdtemp(join(tmpdir(), "phux-tui-mail-"));
  try {
    const executable = join(directory, "pinned phux");
    const log = join(directory, "calls");
    await writeFile(log, "");
    await writeFile(executable, `#!${process.execPath}
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
appendFileSync(process.env.TEST_LOG, JSON.stringify({
  args: process.argv.slice(2), record: process.env.PHUX_CHAT_RECORD,
  address: process.env.PHUX_AGENT_ADDRESS, binary: process.env.PHUX_BIN,
}) + "\\n");
if (process.argv[3] === "release") {
  console.log("{}");
} else if (!existsSync(process.env.TEST_ONCE)) {
  writeFileSync(process.env.TEST_ONCE, "");
  console.log(JSON.stringify({schema:"phux.coord/v1", mail:[{id:"one"}], text:${JSON.stringify(text)}}));
} else {
  process.stdin.resume();
}
`, { mode: 0o700 });
    const env = {
      ...process.env,
      PHUX_TERMINAL_ID: "",
      PHUX_CHAT_RECORD: "",
      PHUX_AGENT_ADDRESS: "",
      PHUX_AGENT_REPORTING: "1",
      PHUX_BIN: executable,
      // The TUI uses PHUX_BIN, not the legacy shell hook's variable or PATH.
      PHUX_AGENT_PHUX_BIN: "/missing/legacy-phux",
      TEST_LOG: log,
      TEST_ONCE: join(directory, "once"),
      ...extra,
    };
    const result = spawnSync(process.execPath, [join(import.meta.dir, "fixtures/tui-mail.ts")], {
      env, encoding: "utf8", timeout: 10_000,
    });
    expect(result.error).toBeUndefined();
    if (result.status !== 0) throw new Error(result.stderr);
    const calls = (await readFile(log, "utf8")).trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
    return { ...JSON.parse(result.stdout), calls, executable };
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test("a TUI-local explicit chat mailbox delivers without pane lifecycle or sidebar writes", async () => {
  const result = await launch({ PHUX_CHAT_RECORD: "/private/chat record", PHUX_AGENT_ADDRESS: "buddy" });
  expect(result.slots).toEqual(["app"]);
  expect(result.lifecycle).toEqual([]);
  expect(result.subscriptions).toEqual(["session.idle"]);
  expect(result.remainingSubscriptions).toBe(0);
  expect(result.errors).toEqual([]);
  expect(result.admitted).toEqual([{
    sessionID: "visible", text, description: "phux mail",
    metadata: { phux_mail_ids: ["one"] }, delivery: "queue", resume: true,
  }]);
  expect(result.calls.length).toBeGreaterThan(0);
  for (const call of result.calls) {
    expect(call).toEqual({
      args: ["mail", "wait", "--json", "--until-stdin-closes", "--timeout", "300"],
      record: "/private/chat record", address: "buddy", binary: result.executable,
    });
  }
});

test("the shipping TUI bundle delivers explicit chat mail but refuses an address alone", async () => {
  const entrypoint = join(import.meta.dir, "../tui.js");
  const delivered = await launch({
    TEST_TUI_ENTRY: entrypoint,
    PHUX_CHAT_RECORD: "/private/chat record",
    PHUX_AGENT_ADDRESS: "buddy",
  });
  expect(delivered.slots).toEqual(["app"]);
  expect(delivered.subscriptions).toEqual(["session.idle"]);
  expect(delivered.remainingSubscriptions).toBe(0);
  expect(delivered.admitted).toEqual([{
    sessionID: "visible", text, description: "phux mail",
    metadata: { phux_mail_ids: ["one"] }, delivery: "queue", resume: true,
  }]);
  expect(delivered.errors).toEqual([]);
  expect(delivered.calls.length).toBeGreaterThan(0);
  for (const call of delivered.calls) {
    expect(call).toEqual({
      args: ["mail", "wait", "--json", "--until-stdin-closes", "--timeout", "300"],
      record: "/private/chat record", address: "buddy", binary: delivered.executable,
    });
  }
  const refused = await launch({ TEST_TUI_ENTRY: entrypoint, PHUX_AGENT_ADDRESS: "ordinary-address" });
  expect(refused.slots).toEqual([]);
  expect(refused.subscriptions).toEqual([]);
  expect(refused.admitted).toEqual([]);
  expect(refused.calls).toEqual([]);
});

test("outside a pane, missing or empty chat identity refuses all integration work", async () => {
  for (const env of [
    {}, { PHUX_AGENT_ADDRESS: "ordinary-address" },
    { PHUX_CHAT_RECORD: "/private/chat" },
    { PHUX_CHAT_RECORD: "", PHUX_AGENT_ADDRESS: "buddy" },
    { PHUX_CHAT_RECORD: "/private/chat", PHUX_AGENT_ADDRESS: "" },
    { PHUX_CHAT_RECORD: "/private/chat", PHUX_AGENT_ADDRESS: "buddy", PHUX_AGENT_REPORTING: "0" },
  ]) {
    const result = await launch(env);
    expect(result.slots).toEqual([]);
    expect(result.subscriptions).toEqual([]);
    expect(result.lifecycle).toEqual([]);
    expect(result.admitted).toEqual([]);
    expect(result.calls).toEqual([]);
  }
});

test("a service refusal releases chat mail through the same record and pinned binary", async () => {
  const result = await launch({
    PHUX_CHAT_RECORD: "/private/chat record", PHUX_AGENT_ADDRESS: "buddy", TEST_REFUSE: "1",
  });
  expect(result.lifecycle).toEqual([]);
  expect(result.calls.find((call: { args: string[] }) => call.args[1] === "release")).toEqual({
    args: ["mail", "release", "--json", "--", "one"],
    record: "/private/chat record", address: "buddy", binary: result.executable,
  });
  expect(result.errors).toHaveLength(1);
});

test("an ordinary pane retains visible-session lifecycle, permissions, and mail", async () => {
  const result = await launch({ PHUX_TERMINAL_ID: "42" });
  expect(result.slots).toEqual(["app", "sidebar.content"]);
  expect(result.subscriptions).toEqual(["session.status", "session.idle", "permission.asked"]);
  expect(result.remainingSubscriptions).toBe(0);
  expect(result.lifecycle).toEqual([
    "construct", "show visible idle", "show visible working", "ask visible",
    "show visible idle", "clear visible", "dispose",
  ]);
  expect(result.admitted).toHaveLength(1);
  expect(result.errors).toEqual([]);
});
