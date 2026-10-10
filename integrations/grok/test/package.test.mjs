import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmod, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ARMS = {
  SessionStart: "start",
  UserPromptSubmit: "working",
  PreToolUse: "tool-start",
  PostToolUse: "tool-end",
  PostToolUseFailure: "tool-end",
  PermissionDenied: "blocked",
  Notification: "blocked",
  Stop: "done",
  StopCancelled: "done",
  SessionEnd: "clear",
};

test("declares a Grok plugin with phux MCP and lifecycle hooks", async () => {
  const manifest = JSON.parse(await readFile(new URL("../.grok-plugin/plugin.json", import.meta.url)));
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url)));
  const mcp = JSON.parse(await readFile(new URL("../.mcp.json", import.meta.url)));
  const hooks = JSON.parse(await readFile(new URL("../hooks/hooks.json", import.meta.url)));
  const marketplace = JSON.parse(
    await readFile(new URL("../../../.grok-plugin/marketplace.json", import.meta.url)),
  );
  assert.equal(manifest.name, "phux");
  assert.equal(manifest.version, pkg.version);
  assert.equal(pkg.private, true);
  assert.deepEqual(mcp.mcpServers.phux, { command: "phux", args: ["mcp"] });
  assert.equal(marketplace.plugins[0].source, "./integrations/grok");
  assert.equal(marketplace.plugins[0].version, pkg.version);
  assert.deepEqual(Object.keys(hooks.hooks).sort(), Object.keys(ARMS).sort());
  for (const [event, groups] of Object.entries(hooks.hooks)) {
    for (const group of groups) {
      for (const hook of group.hooks) {
        assert.equal(hook.command, "sh");
        assert.equal(hook.args[0], "${GROK_PLUGIN_ROOT}/scripts/phux-hook.sh");
        assert.equal(hook.args[1], ARMS[event]);
        assert.equal(hook.timeout, 5);
      }
    }
  }
});

test("session start declares Grok on the hosting pane", async () => {
  const temp = await mkdtemp(join(tmpdir(), "phux-grok-hook-"));
  const log = join(temp, "log");
  const fake = join(temp, "phux");
  await writeFile(
    fake,
    `#!/bin/sh
printf '%s\\n' "$*" >> "$PHUX_TEST_LOG"
case "$1 \${2:-}" in
  "status --json") printf '%s\\n' '{"features":{"resource_kinds":["agent_session"]}}'; exit 0 ;;
  "agent hook-payload") cat >/dev/null; printf '%s\\n' "$FAKE_FIELDS"; exit 0 ;;
  "agent hook-transcript") cat >/dev/null; exit 0 ;;
esac
exit 0
`,
  );
  await chmod(fake, 0o755);
  const script = fileURLToPath(new URL("../scripts/phux-hook.sh", import.meta.url));
  execFileSync(script, ["start"], {
    env: {
      ...process.env,
      PHUX_TERMINAL_ID: "42",
      PHUX_AGENT_PHUX_BIN: fake,
      PHUX_TEST_LOG: log,
      FAKE_FIELDS: "sess-1 session_start - - 0 - startup",
      PHUX_AGENT_TRANSCRIPT: "0",
    },
    input: "{}",
  });
  const lines = (await readFile(log, "utf8")).trim().split("\n");
  assert.ok(lines.includes("agent set @42 --name grok --kind grok"));
  assert.ok(lines.some((line) => line.startsWith("agent session open @42 --provider grok")));
});
