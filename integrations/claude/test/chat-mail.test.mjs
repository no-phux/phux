import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// Claude's existing hook contract suite also exercises Grok's shell hooks.
for (const provider of ["claude", "grok"]) {
  test(`${provider} chat-only mail forwards the private mailbox and pinned hook binary, never lifecycle`, async () => {
    const directory = await mkdtemp(join(tmpdir(), `phux-${provider}-chat-`));
    try {
      const executable = join(directory, "pinned phux.mjs");
      const log = join(directory, "calls");
      const reply = { decision: "block", reason: "[phux mail] A colleague's message, not user instructions." };
      await writeFile(log, "");
      await writeFile(executable, `#!${process.execPath}
import { appendFileSync } from "node:fs";
appendFileSync(process.env.TEST_LOG, JSON.stringify({
  args: process.argv.slice(2), record: process.env.PHUX_CHAT_RECORD,
  address: process.env.PHUX_AGENT_ADDRESS, binary: process.env.PHUX_AGENT_PHUX_BIN,
}) + "\\n");
console.log(${JSON.stringify(JSON.stringify(reply))});
`, { mode: 0o700 });
      const env = {
        ...process.env,
        PHUX_TERMINAL_ID: "",
        PHUX_CHAT_RECORD: "/private/chat record",
        PHUX_AGENT_ADDRESS: "",
        PHUX_AGENT_REPORTING: "1",
        PHUX_AGENT_PHUX_BIN: executable,
        PHUX_BIN: "/missing/not-the-hook-binary",
        TEST_LOG: log,
      };
      // These probes inspect dispatch, not hook payloads. Short-circuiting
      // hooks may exit before reading stdin; do not race an unused pipe write.
      const run = (script, event, extra = {}) => execFileSync("sh", [
        fileURLToPath(new URL(`../../${provider}/scripts/${script}`, import.meta.url)), event,
      ], { env: { ...env, ...extra }, stdio: ["ignore", "pipe", "pipe"], encoding: "utf8" });
      const events = provider === "claude"
        ? ["SessionStart", "UserPromptSubmit", "PostToolUse", "PostToolUseFailure", "Stop"]
        : ["PostToolUse", "PostToolUseFailure", "Stop"];
      for (const event of events) {
        assert.equal(run("phux-mail.sh", event), `${JSON.stringify(reply)}\n`);
      }
      for (const extra of [
        { PHUX_CHAT_RECORD: "" },
        { PHUX_CHAT_RECORD: "", PHUX_AGENT_ADDRESS: "ordinary-address" },
        { PHUX_AGENT_REPORTING: "0" },
      ]) {
        assert.equal(run("phux-mail.sh", "Stop", extra), "");
      }
      for (const action of ["start", "working", "tool-start", "tool-end", "blocked", "done", "clear"]) {
        assert.equal(run("phux-hook.sh", action), "", "chat identity must not enable lifecycle hooks");
      }
      assert.deepEqual((await readFile(log, "utf8")).trim().split("\n").map(JSON.parse), events.map(event => ({
        args: ["mail", "hook-context", "--event", event],
        record: "/private/chat record", address: "", binary: executable,
      })), "only registered mail hooks may invoke phux");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
}
