import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = new URL("../", import.meta.url);

test("declares one marketplace-ready plugin with phux MCP and bounded lifecycle hooks", async () => {
  const manifest = JSON.parse(await readFile(new URL("../.claude-plugin/plugin.json", import.meta.url)));
  const packageManifest = JSON.parse(await readFile(new URL("../package.json", import.meta.url)));
  const mcp = JSON.parse(await readFile(new URL("../.mcp.json", import.meta.url)));
  const hooks = JSON.parse(await readFile(new URL("../hooks/hooks.json", import.meta.url)));
  assert.equal(manifest.name, "phux");
  assert.equal(manifest.version, packageManifest.version);
  assert.deepEqual(mcp.mcpServers.phux, { command: "phux", args: ["mcp"] });
  assert.deepEqual(Object.keys(hooks.hooks).sort(), [
    "Notification", "PermissionRequest", "SessionEnd", "SessionStart",
  ]);
  for (const groups of Object.values(hooks.hooks)) {
    for (const group of groups) {
      for (const hook of group.hooks) {
        assert.equal(hook.command, "sh");
        assert.match(hook.args[0], /^\$\{CLAUDE_PLUGIN_ROOT\}/);
        assert.equal(hook.timeout, 5);
      }
    }
  }
});

test("hook script emits exact identity, attention, and cleanup argv without model output", async () => {
  const temp = await mkdtemp(join(tmpdir(), "phux-claude-hook-"));
  const fake = join(temp, "phux");
  const log = join(temp, "argv.log");
  await writeFile(fake, `#!/bin/sh\nprintf '%s\\n' "$*" >> "$PHUX_TEST_LOG"\n`);
  await chmod(fake, 0o755);
  const script = fileURLToPath(new URL("../scripts/phux-hook.sh", import.meta.url));
  const env = {
    ...process.env,
    PHUX_AGENT_PHUX_BIN: fake,
    PHUX_TERMINAL_ID: "42",
    PHUX_TEST_LOG: log,
  };
  try {
    for (const action of ["start", "blocked", "clear"]) {
      const output = execFileSync("sh", [script, action], { cwd: root, env, encoding: "utf8" });
      assert.equal(output, "");
    }
    assert.deepEqual((await readFile(log, "utf8")).trim().split("\n"), [
      "agent set @42 --name claude --kind claude",
      "ask @42 Claude needs attention",
      "agent clear @42",
    ]);
    assert.equal(execFileSync("sh", [script, "start"], {
      cwd: root,
      env: { ...env, PHUX_TERMINAL_ID: "" },
      encoding: "utf8",
    }), "");
  } finally {
    await rm(temp, { recursive: true, force: true });
  }
});
