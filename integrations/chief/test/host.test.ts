import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import type { AssistantMessage } from "@earendil-works/pi-ai";

import { LoginBroker, type LoginStep } from "../src/auth.js";
import { applyChanges } from "../src/partial.js";
import { chiefPaths, defaultConfig, loadConfig, parseConfig, splitModel } from "../src/config.js";
import { ErrorIntake, investigationPrompt, serverLogFindings, template } from "../src/diagnostics.js";
import { ensure, type EnsureCli } from "../src/ensure.js";
import { ChiefStream, pages, type StreamCli } from "../src/phux-stream.js";
import type { AgentRecord } from "@phux/integration-runtime/schemas";

const tmp = () => mkdtempSync(join(tmpdir(), "chief-test-"));

test("config: defaults, overrides, and refusals by field name", () => {
  const base = defaultConfig({});
  assert.equal(base.model, "anthropic/claude-sonnet-5");
  assert.equal(base.yolo, false);
  const config = parseConfig({ host: "mini", yolo: true, ownedRepos: ["no-phux/phux"], selfHeal: "notify" }, base);
  assert.deepEqual([config.host, config.yolo, config.ownedRepos, config.selfHeal], ["mini", true, ["no-phux/phux"], "notify"]);
  assert.throws(() => parseConfig({ yolo: "yes" }, base), /yolo/);
  assert.throws(() => parseConfig({ model: "sonnet" }, base), /provider\/modelId/);
  assert.throws(() => parseConfig({ ownedRepos: ["../x"] }, base), /ownedRepos/);
  assert.deepEqual(splitModel("openrouter/anthropic/claude"), { provider: "openrouter", modelId: "anthropic/claude" });
});

test("config: PHUX_CHIEF_YOLO overrides the file in both directions", () => {
  const dir = tmp();
  const env = { PHUX_CHIEF_CONFIG: join(dir, "chief.json"), PHUX_CHIEF_HOME: dir };
  writeFileSync(env.PHUX_CHIEF_CONFIG, JSON.stringify({ yolo: true }));
  assert.equal(loadConfig(chiefPaths(env), env).yolo, true);
  assert.equal(loadConfig(chiefPaths({ ...env, PHUX_CHIEF_YOLO: "0" }), { ...env, PHUX_CHIEF_YOLO: "0" }).yolo, false);
});

test("intake: server ERROR lines and panics fingerprint by template, not by ids", () => {
  const log = [
    "2026-10-20T20:05:28Z ERROR phux_server::runtime::client: accept failed error=Socket is not connected (os error 57)",
    "2026-10-20T20:05:29Z ERROR phux_server::runtime::client: accept failed error=Socket is not connected (os error 57)",
    "2026-10-20T20:06:00Z  INFO phux_server: fine",
    "thread 'main' panicked at crates/phux-server/src/x.rs:10:5:",
  ].join("\n");
  const findings = serverLogFindings(log);
  assert.equal(findings.length, 2);
  assert.equal(template("pane 12 at /tmp/a/b failed \"x\" 0xdeadbeef"), template("pane 7 at /var/c failed \"y\" 0x1"));
  assert.match(investigationPrompt(findings, ["no-phux/phux"]), /no-phux\/phux[\s\S]*accept failed/);
});

test("intake: only news, a first run starts quiet, and investigations are rate limited", async () => {
  const dir = tmp();
  const serverLog = join(dir, "server.log");
  writeFileSync(serverLog, "2026 ERROR old: before the chief started\n");
  let now = 1_000_000;
  const runs: string[][] = [];
  const intake = new ErrorIntake({
    statePath: join(dir, "intake.json"), ownedRepos: ["o/r"], serverLog, crashDir: join(dir, "none"), reportsDir: join(dir, "none"),
    now: () => now,
    run: async (_file, args) => {
      runs.push([...args]);
      return JSON.stringify([{ databaseId: 9, workflowName: "ci", displayTitle: "t", url: "u", createdAt: new Date(now).toISOString(), headSha: "abcdef0123" }]);
    },
  });
  const first = await intake.scan();
  assert.deepEqual(first.map((finding) => finding.source), ["ci"], "existing log lines are not news");
  writeFileSync(serverLog, "2026 ERROR old: before the chief started\n2026 ERROR new: it broke\n");
  const second = await intake.scan();
  assert.deepEqual(second.map((finding) => finding.title), ["phux server: new: it broke"], "the CI run is not repeated");
  assert.ok(runs[0]!.includes("o/r"));
  assert.equal(intake.claimInvestigation(60), true);
  now += 30 * 60_000;
  assert.equal(intake.claimInvestigation(60), false);
  now += 31 * 60_000;
  assert.equal(intake.claimInvestigation(60), true);
});

test("login: steps publish, a prompt waits for its answer, a wrong id is refused", async () => {
  const steps: LoginStep[] = [];
  let answered = "";
  const models = {
    getProvider: (id: string) => (id === "acme" ? { auth: { oauth: {} } } : undefined),
    login: async (_provider: string, _type: string, interaction: { notify(e: unknown): void; prompt(p: unknown): Promise<string> }) => {
      interaction.notify({ type: "auth_url", url: "https://acme/authorize", instructions: "paste the code" });
      answered = await interaction.prompt({ type: "manual_code", message: "Paste the code" });
      return { type: "oauth", access: "a", refresh: "r", expires: 1 };
    },
  };
  const broker = new LoginBroker(models as never, (_record, step) => steps.push(step));
  const flow = broker.start("acme", undefined);
  const id = broker.current!.login;
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(broker.answer("other", "x"), false);
  assert.equal(broker.answer(id, "CODE-123"), true);
  await flow;
  assert.equal(answered, "CODE-123");
  assert.deepEqual(steps.map((step) => step.step), ["started", "url", "prompt", "done"]);
  await assert.rejects(broker.start("nope", undefined), /unknown provider/);
});

test("streaming changes fold into the partial assistant message", () => {
  const message = { role: "assistant", content: [], timestamp: 5 } as unknown as AssistantMessage;
  assert.equal(applyChanges(message, [{ type: "text_start", contentIndex: 0, block: { type: "text", text: "" } }, { type: "text_delta", contentIndex: 0, delta: "Hel" }]), "text_delta");
  applyChanges(message, [{ type: "text_delta", contentIndex: 0, delta: "lo" }]);
  assert.deepEqual(message.content, [{ type: "text", text: "Hello" }]);
  assert.equal(applyChanges(message, [{ type: "toolcall_delta", contentIndex: 1, path: [], delta: "{" }]), undefined);
});

test("stream: declares, tags and opens the pane, then emits in order; oversize chief records are dropped", async () => {
  const calls: string[] = [];
  const errors: unknown[] = [];
  // Methods on a prototype, as on PhuxCli, so a spread copy would lose them.
  const cli: StreamCli = Object.create({
    agentSet: async (target: string, record: AgentRecord) => { calls.push(`set ${target} ${record.name}/${record.kind}/${record.state}`); return record; },
    tag: async (action, target, tags) => { calls.push(`tag ${action} ${target} ${(tags ?? []).join(",")}`); return []; },
    agentSessionOpen: async (target, options) => { calls.push(`open ${target} ${options.provider}`); return { schema_version: 1, resource: "@99", parent: target, provider: options.provider, native_id: options.nativeId ?? null } as never; },
    agentEmit: async (target, type, options) => { calls.push(`emit ${target} ${type} ${JSON.stringify(options?.data ?? {})}`); return {} as never; },
    agentSessionClose: async (target) => { calls.push(`close ${target}`); return {} as never; },
    agentClear: async (target: string) => { calls.push(`clear ${target}`); },
  } satisfies StreamCli);
  const sessionFile = join(tmp(), "agent-session.json");
  writeFileSync(sessionFile, JSON.stringify({ pane: "@4", resource: "@50" }));
  const stream = new ChiefStream(cli, "@4", "chief", (error) => errors.push(error), sessionFile);
  await stream.bind();
  assert.deepEqual(JSON.parse(readFileSync(sessionFile, "utf8")), { pane: "@4", resource: "@99" }, "the new session is remembered");
  assert.deepEqual(calls.splice(2, 2), ["emit @50 session_end {}", "close @50"], "a crashed predecessor's session is closed first");
  await stream.emit("stop", { reply: "done", reply_title: "Chief" });
  await stream.state("done");
  await stream.chief("status", { yolo: false });
  await stream.chief("huge", { blob: "x".repeat(20_000) });
  await stream.close();
  assert.deepEqual(calls.slice(0, 3), ["set @4 chief/pi/idle", "tag add @4 chief", "open @4 pi"]);
  const emits = calls.filter((call) => call.startsWith("emit") && !call.includes("session_start"));
  assert.match(emits[0]!, /^emit @99 stop .*"reply":"done"/);
  assert.match(emits[1]!, /^emit @99 provider_raw .*"schema":"phux.chief\/v1","event":"status"/);
  assert.ok(calls.includes("set @4 chief/pi/done"), "a turn end declares done");
  assert.equal(calls.filter((call) => call.includes("huge")).length, 0);
  assert.equal(errors.length, 1);
  assert.deepEqual(calls.slice(-3), ["emit @99 session_end {}", "close @99", "clear @4"]);
  assert.deepEqual(pages([1, 2, 3], 6).map((page) => page.length), [2, 1]);
});

test("ensure: elsewhere, already running, and a fresh start that tags the new pane", async () => {
  const dir = tmp();
  const env = { PHUX_CHIEF_CONFIG: join(dir, "chief.json"), PHUX_CHIEF_HOME: dir };
  writeFileSync(env.PHUX_CHIEF_CONFIG, JSON.stringify({ host: "mac" }));
  const tagged: string[] = [];
  let running = false;
  const cli = {
    status: async () => ({}),
    tag: async (action: string, target: string) => {
      if (action === "add") { tagged.push(target); return []; }
      return running ? [{ terminal: "@7", tagsText: "chief" }] : [];
    },
    create: async () => ({ session: "chief", terminal_id: 12 }),
    spawn: async () => { throw new Error("unused"); },
    agentList: async () => ({ agents: [] }),
  } as unknown as EnsureCli;
  assert.deepEqual(await ensure(cli, env, "mini"), { kind: "elsewhere" });
  assert.deepEqual(await ensure(cli, env, "mac"), { kind: "started", pane: "@12" });
  assert.deepEqual(tagged, ["@12"]);
  running = true;
  assert.deepEqual(await ensure(cli, env, "mac"), { kind: "running", pane: "@7" });
  writeFileSync(join(dir, "disabled"), "");
  assert.deepEqual(await ensure(cli, env, "mac"), { kind: "disabled" });
});
