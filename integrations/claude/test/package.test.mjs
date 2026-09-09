import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = new URL("../", import.meta.url);
const script = fileURLToPath(new URL("../scripts/phux-hook.sh", import.meta.url));

// The eight registrations and the arm each one dispatches to.
const ARMS = {
  SessionStart: "start",
  UserPromptSubmit: "working",
  PreToolUse: "tool-start",
  PostToolUse: "tool-end",
  PermissionRequest: "blocked",
  Notification: "blocked",
  Stop: "done",
  SessionEnd: "clear",
};

test("declares one marketplace-ready plugin with phux MCP and bounded lifecycle hooks", async () => {
  const manifest = JSON.parse(await readFile(new URL("../.claude-plugin/plugin.json", import.meta.url)));
  const packageManifest = JSON.parse(await readFile(new URL("../package.json", import.meta.url)));
  const mcp = JSON.parse(await readFile(new URL("../.mcp.json", import.meta.url)));
  const hooks = JSON.parse(await readFile(new URL("../hooks/hooks.json", import.meta.url)));
  assert.equal(manifest.name, "phux");
  assert.equal(manifest.version, packageManifest.version);
  assert.deepEqual(mcp.mcpServers.phux, { command: "phux", args: ["mcp"] });
  assert.deepEqual(Object.keys(hooks.hooks).sort(), Object.keys(ARMS).sort());
  for (const [event, groups] of Object.entries(hooks.hooks)) {
    for (const group of groups) {
      for (const hook of group.hooks) {
        assert.equal(hook.command, "sh");
        assert.match(hook.args[0], /^\$\{CLAUDE_PLUGIN_ROOT\}/);
        assert.equal(hook.args[1], ARMS[event]);
        assert.equal(hook.timeout, 5);
      }
    }
  }
  assert.equal(hooks.hooks.Notification[0].matcher, "permission_prompt|idle_prompt|elicitation_dialog");
});

/**
 * A fake `phux` that logs every argv line, answers the capability probe from
 * `FAKE_FEATURES`, stands in for `agent hook-payload` with the canned
 * `FAKE_FIELDS` line (the real helper is pinned in the phux crate), and logs
 * whatever an `emit --data -` was fed on stdin. A phux old enough to lack the
 * helper is modelled by an empty `FAKE_FIELDS`.
 */
async function fakePhux(temp) {
  const fake = join(temp, "phux");
  await writeFile(fake, `#!/bin/sh
printf '%s\\n' "$*" >> "$PHUX_TEST_LOG"
case "$1 \${2:-}" in
  "status --json") printf '{"running":true,"features":%s}\\n' "\${FAKE_FEATURES:-[]}"; exit 0 ;;
  "agent hook-payload") cat > /dev/null; [ -z "\${FAKE_FIELDS:-}" ] || printf '%s\\n' "$FAKE_FIELDS"; exit 0 ;;
esac
case "$*" in *"--data -") printf 'stdin:%s\\n' "$(cat)" >> "$PHUX_TEST_LOG" ;; esac
exit 0
`);
  await chmod(fake, 0o755);
  return fake;
}

async function driven(env, runs) {
  const temp = await mkdtemp(join(tmpdir(), "phux-claude-hook-"));
  const log = join(temp, "argv.log");
  const fake = await fakePhux(temp);
  try {
    const lines = [];
    for (const [action, payload, extra] of runs) {
      const output = execFileSync("sh", [script, action], {
        cwd: root,
        env: { ...process.env, PHUX_AGENT_PHUX_BIN: fake, PHUX_TERMINAL_ID: "42", PHUX_TEST_LOG: log, ...env, ...extra },
        encoding: "utf8",
        input: payload,
      });
      assert.equal(output, "", `${action} must print nothing back to Claude`);
      lines.push((await readFile(log, "utf8")).trim().split("\n"));
      await writeFile(log, "");
    }
    return lines;
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
}

test("against a server without resource kinds the arms keep the identity, ask, and clear argv", async () => {
  const fields = "sess-1 PreToolUse Bash permission_prompt 11 clear startup";
  const [start, working, toolStart, toolEnd, blocked, done, clear, silent] = await driven(
    { FAKE_FEATURES: '["report_agent_state"]', FAKE_FIELDS: fields },
    [
      ["start", "{}"], ["working", "{}"], ["tool-start", "{}"], ["tool-end", "{}"],
      ["blocked", "{}"], ["done", "{}"], ["clear", "{}"],
      ["start", "{}", { PHUX_TERMINAL_ID: "" }],
    ],
  );
  assert.deepEqual(start, ["status --json", "agent hook-payload", "agent set @42 --name claude --kind claude"]);
  assert.deepEqual(working, ["status --json", "agent hook-payload"]);
  assert.deepEqual(toolStart, ["status --json", "agent hook-payload"]);
  assert.deepEqual(toolEnd, ["status --json", "agent hook-payload"]);
  assert.deepEqual(blocked, ["status --json", "agent hook-payload", "ask @42 Claude needs attention"]);
  assert.deepEqual(done, ["status --json", "agent hook-payload"]);
  assert.deepEqual(clear, ["status --json", "agent hook-payload", "agent clear @42"]);
  assert.deepEqual(silent, [""], "no pane, no calls");
});

test("against a server with resource kinds the arms open, feed, and close the session stream", async () => {
  const streaming = { FAKE_FEATURES: '["report_agent_state","resource_kinds"]' };
  const f = (event, tool, kind, chars, reason, source) => ({
    FAKE_FIELDS: `sess-1 ${event} ${tool} ${kind} ${chars} ${reason} ${source}`,
  });
  const lines = await driven(streaming, [
    ["start", "{}", f("SessionStart", "-", "-", 0, "-", "startup")],
    ["start", "{}", f("SessionStart", "-", "-", 0, "-", "compact")],
    ["working", "{}", f("UserPromptSubmit", "-", "-", 18, "-", "-")],
    ["tool-start", "{}", f("PreToolUse", "Bash", "-", 0, "-", "-")],
    ["tool-end", "{}", f("PostToolUse", "mcp__phux__phux_ls", "-", 0, "-", "-")],
    ["blocked", "{}", f("PermissionRequest", "Bash", "-", 0, "-", "-")],
    ["blocked", "{}", f("Notification", "-", "permission_prompt", 0, "-", "-")],
    ["blocked", "{}", f("Notification", "-", "elicitation_dialog", 0, "-", "-")],
    ["blocked", "{}", f("Notification", "-", "idle_prompt", 0, "-", "-")],
    ["done", "{}", f("Stop", "-", "-", 0, "-", "-")],
    ["clear", "{}", f("SessionEnd", "-", "-", 0, "prompt_input_exit", "-")],
  ]);
  const probe = ["status --json", "agent hook-payload"];
  assert.deepEqual(lines, [
    [...probe, "agent set @42 --name claude --kind claude",
      "agent session open @42 --provider claude --native-id=sess-1", "agent emit @42 --type session_start"],
    [...probe, "agent set @42 --name claude --kind claude"],
    [...probe, 'agent emit @42 --type prompt --data {"chars":18}'],
    [...probe, 'agent emit @42 --type tool_start --data {"tool_name":"Bash"}'],
    [...probe, 'agent emit @42 --type tool_end --data {"tool_name":"mcp__phux__phux_ls"}'],
    [...probe, "agent emit @42 --type ask", "ask @42 Claude needs attention"],
    [...probe, 'agent emit @42 --type notification --data {"kind":"permission"}', "ask @42 Claude needs attention"],
    [...probe, 'agent emit @42 --type notification --data {"kind":"elicitation"}', "ask @42 Claude needs attention"],
    [...probe, 'agent emit @42 --type notification --data {"kind":"idle"}', "ask @42 Claude needs attention"],
    [...probe, "agent emit @42 --type stop"],
    [...probe, 'agent emit @42 --type session_end --data {"reason":"prompt_input_exit"}',
      "agent session close @42", "agent clear @42"],
  ]);
});

test("payload text never reaches an argv line, and the raw record is opt-in and precedes session_end", async () => {
  const payload = '{"session_id":"sess-1","hook_event_name":"PreToolUse","tool_name":"Bash","prompt":"PROMPT-MARKER","tool_input":{"command":"INPUT-MARKER"}}';
  const fields = { FAKE_FIELDS: "sess-1 PreToolUse Bash - 13 - -" };
  for (const features of ['["report_agent_state"]', '["report_agent_state","resource_kinds"]']) {
    const lines = await driven({ FAKE_FEATURES: features, ...fields }, [
      ["start", payload], ["working", payload], ["tool-start", payload], ["tool-end", payload],
      ["blocked", payload], ["done", payload], ["clear", payload],
    ]);
    for (const run of lines) {
      assert.ok(run.every((line) => !line.includes("MARKER")), `leaked: ${run}`);
    }
  }
  const [toolStart, clear] = await driven(
    { FAKE_FEATURES: '["resource_kinds"]', PHUX_AGENT_EMIT_RAW: "1", ...fields },
    [["tool-start", payload], ["clear", payload, { FAKE_FIELDS: "sess-1 SessionEnd - - 0 other -" }]],
  );
  assert.deepEqual(toolStart, [
    "status --json", "agent hook-payload",
    'agent emit @42 --type tool_start --data {"tool_name":"Bash"}',
    "agent emit @42 --type provider_raw --data -", `stdin:${payload}`,
  ]);
  assert.deepEqual(clear, [
    "status --json", "agent hook-payload",
    "agent emit @42 --type provider_raw --data -", `stdin:${payload}`,
    'agent emit @42 --type session_end --data {"reason":"other"}',
    "agent session close @42", "agent clear @42",
  ]);
  const [legacyRaw] = await driven(
    { FAKE_FEATURES: "[]", PHUX_AGENT_EMIT_RAW: "1", ...fields },
    [["tool-start", payload]],
  );
  assert.ok(legacyRaw.every((line) => !line.includes("provider_raw")), "raw has nowhere to go without a stream");
});

test("a phux without the payload helper still runs the every-server arms", async () => {
  const [start, blocked, clear] = await driven(
    { FAKE_FEATURES: "[]", FAKE_FIELDS: "" },
    [["start", "{}"], ["blocked", "{}"], ["clear", "{}"]],
  );
  assert.deepEqual(start, ["status --json", "agent hook-payload", "agent set @42 --name claude --kind claude"]);
  assert.deepEqual(blocked, ["status --json", "agent hook-payload", "ask @42 Claude needs attention"]);
  assert.deepEqual(clear, ["status --json", "agent hook-payload", "agent clear @42"]);
});
