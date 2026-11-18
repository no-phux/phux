import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

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
  // Grok feeds the model only tool-result context and a Stop block.
  const mailEvents = ["PostToolUse", "PostToolUseFailure", "Stop"];
  for (const [event, groups] of Object.entries(hooks.hooks)) {
    for (const group of groups) {
      const [lifecycle, mail, ...rest] = group.hooks;
      assert.deepEqual(lifecycle.args, ["${GROK_PLUGIN_ROOT}/scripts/phux-hook.sh", ARMS[event]]);
      assert.deepEqual(rest, []);
      if (mailEvents.includes(event)) {
        assert.deepEqual(mail.args, ["${GROK_PLUGIN_ROOT}/scripts/phux-mail.sh", event]);
      } else {
        assert.equal(mail, undefined, `${event} is passive`);
      }
      for (const hook of group.hooks) {
        assert.equal(hook.command, "sh");
        assert.equal(hook.timeout, 5);
      }
    }
  }
});
