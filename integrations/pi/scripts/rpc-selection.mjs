import assert from "node:assert/strict";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

/** Real Pi RPC UI protocol against the smoke's private phux server; no LLM. */
export async function verifyRpcSelection(temp, env, target) {
  const childEnv = {
    ...env,
    PI_CODING_AGENT_DIR: join(temp, "rpc-pi"),
    PI_CODING_AGENT_SESSION_DIR: join(temp, "rpc-sessions"),
  };
  delete childEnv.PHUX_TERMINAL_ID;
  const child = spawn(process.env.PI_BIN ?? "pi", [
    "--mode", "rpc", "--offline", "--no-skills", "--no-context-files",
    "--no-extensions", "--approve", "--extension",
    fileURLToPath(new URL("../extensions/index.ts", import.meta.url)),
  ], { cwd: temp, env: childEnv, stdio: ["pipe", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr = `${stderr}${chunk}`.slice(-8192); });
  child.on("error", (error) => { stderr = error.message; });
  const exited = new Promise((resolve) => child.once("close", resolve));
  const timeout = setTimeout(() => child.kill("SIGKILL"), 30_000);
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  const send = (record) => child.stdin.write(`${JSON.stringify(record)}\n`);
  const next = async (predicate) => {
    for (;;) {
      const line = await lines.next();
      assert.equal(line.done, false, `Pi RPC ended before its expected response: ${stderr}`);
      const record = JSON.parse(line.value);
      if (predicate(record)) return record;
    }
  };
  try {
    send({ id: "cancel", type: "prompt", message: "/phux" });
    const cancelled = await next(isSelect);
    send({ type: "extension_ui_response", id: cancelled.id, cancelled: true });
    assert.equal((await next((record) => record.id === "cancel")).success, true);

    send({ id: "choose", type: "prompt", message: "/phux" });
    const selection = await next(isSelect);
    const option = selection.options.find((text) => text.includes(` ${target} -`));
    assert.ok(option, `offered RPC options must contain ${target}`);
    send({ type: "extension_ui_response", id: selection.id, value: option });
    assert.equal((await next((record) => record.id === "choose")).success, true);

    send({ id: "status", type: "prompt", message: "/phux-status" });
    const notice = await next((record) => record.type === "extension_ui_request" && record.method === "notify");
    assert.ok(notice.message.includes(` ${target}`), "status must expose the selected pane");
    assert.equal((await next((record) => record.id === "status")).success, true);

    // Pi intentionally defers disk creation until a conversation message exists.
    // Inspect its actual persisted-entry tree without starting an LLM turn.
    send({ id: "tree", type: "get_tree" });
    const tree = await next((record) => record.id === "tree");
    assert.equal(tree.success, true);
    const selections = treeEntries(tree.data.tree)
      .filter((entry) => entry.type === "custom" && entry.customType === "phux-target");
    assert.equal(selections.length, 1, "cancelled selection must not persist");
    assert.equal(selections[0].data.selector, target);
    process.stdout.write(`verified real RPC /phux cancellation, selection and persistence for ${target}\n`);
  } finally {
    child.kill("SIGTERM");
    await exited;
    clearTimeout(timeout);
    await lines.return();
  }
}

function treeEntries(nodes) {
  return nodes.flatMap((node) => [node.entry, ...treeEntries(node.children)]);
}

function isSelect(record) {
  return record.type === "extension_ui_request" && record.method === "select";
}
