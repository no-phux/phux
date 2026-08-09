import assert from "node:assert/strict";
import test from "node:test";

import PhuxPlugin, { PhuxCli } from "../dist/index.js";

function completed(stdout = "", exitCode = 0) {
  return { termination: "completed", exitCode, stdout, stderr: "" };
}

function option(args, name) {
  const at = args.indexOf(name);
  // `indexOf` returns -1 when the flag is absent, and -1 + 1 is 0, which would
  // silently report argv[0] as the value. Optional flags are the normal case
  // now that the record is identity-only (phux-w7z2.38).
  return at === -1 ? undefined : args[at + 1];
}

test("documented session status events declare owner-labelled identity, and never a state", async () => {
  const requests = [];
  let record;
  const cli = new PhuxCli({ runner: async (request) => {
    requests.push(request);
    if (request.args[0] === "agent" && request.args[1] === "show") {
      return completed(JSON.stringify({ schema_version: 1, agents: [] }));
    }
    if (request.args[0] !== "agent" || request.args[1] !== "set") {
      throw new Error(`unexpected lifecycle request: ${request.args.join(" ")}`);
    }
    record = {
      name: option(request.args, "--name"),
      kind: option(request.args, "--kind"),
      state: option(request.args, "--state"),
      attention: option(request.args, "--attention"),
      session: option(request.args, "--session"),
    };
    return completed(`@5\t${JSON.stringify(record)}`);
  } });
  const hooks = await PhuxPlugin({}, { cli, env: { PHUX_TARGET: "@5" }, lifecycleTimeoutMs: 321 });

  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "session-public-1", status: { type: "busy" } },
  } });
  // Identity only: a declared state outranks the server's derivation for the
  // record's lifetime (L3.md 3.7, ADR-0046 point 8), which stood
  // `rules/opencode.toml` down on every pane running this plugin
  // (phux-w7z2.38).
  assert.deepEqual(record, {
    name: "opencode",
    kind: "opencode",
    state: undefined,
    attention: undefined,
    session: "opencode:session-public-1",
  });

  const writesAfterBusy = requests.filter((request) => request.args[1] === "set").length;
  await hooks.event({ event: {
    type: "session.idle",
    properties: { sessionID: "session-public-1" },
  } });
  // A turn boundary declares nothing new. Rewriting identity here would carry
  // `state: "unknown"` and clobber the derivation, publishing a
  // `working -> unknown` edge that `phux agent wait` reads as a departure
  // (phux-w7z2.37).
  assert.equal(
    requests.filter((request) => request.args[1] === "set").length,
    writesAfterBusy,
    "a turn boundary must not rewrite the record",
  );
  assert.equal(requests.every((request) => request.timeoutMs === 321), true);
  assert.equal(requests.every((request) => request.signal instanceof AbortSignal), true);
  await hooks.dispose();
});

test("session deletion resolves a session/window selector and clears only the owned canonical pane", async () => {
  const requests = [];
  let record;
  let exposeOwner = true;
  const cli = new PhuxCli({ runner: async (request) => {
    requests.push(request);
    if (request.args[0] !== "agent") throw new Error("expected agent command");
    if (request.args[1] === "set") {
      record = {
        name: option(request.args, "--name"),
        kind: option(request.args, "--kind"),
        state: option(request.args, "--state"),
        attention: option(request.args, "--attention"),
        session: option(request.args, "--session"),
      };
      return completed(`@6\t${JSON.stringify(record)}`);
    }
    if (request.args[1] === "show") {
      const observed = exposeOwner ? record : { ...record, session: "opencode:someone-else" };
      return completed(JSON.stringify({
        schema_version: 1,
        agents: [{
          terminal: "@6",
          session: "shared",
          window: "window-0",
          agent: { id: "declared", label: "opencode", kind: "declared" },
          state: "idle",
          confidence: 1,
          attention: "low",
          title: null,
          cwd: null,
          sources: [{ kind: "agent_record", signal: "declared", confidence: 1, observed: JSON.stringify(observed) }],
          explanation: "declared record",
        }],
      }));
    }
    if (request.args[1] === "clear") return completed("@6\t-");
    throw new Error(`unexpected agent command: ${request.args.join(" ")}`);
  } });
  const hooks = await PhuxPlugin({}, { cli, env: { PHUX_TARGET: "shared:window-0" } });

  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "owned", status: { type: "busy" } },
  } });
  await hooks.event({ event: {
    type: "session.deleted",
    properties: { info: { id: "owned" } },
  } });
  const show = requests.find((request) => request.args[1] === "show");
  const clear = requests.find((request) => request.args[1] === "clear");
  assert.equal(show.args.at(-1), "shared:window-0", "agent show receives the broad selector");
  assert.equal(clear.args.at(-1), "@6", "agent clear receives the resolved canonical pane selector");

  requests.length = 0;
  exposeOwner = false;
  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "not-owner", status: { type: "busy" } },
  } });
  await hooks.dispose();
  assert.equal(requests.some((request) => request.args[1] === "show"), true);
  assert.equal(requests.some((request) => request.args[1] === "clear"), false, "dispose preserves a replacement owner's declaration");
});

test("dispose isolates owned sessions when the first cleanup fails", async () => {
  const requests = [];
  const errors = [];
  let latestRecord;
  let showCount = 0;
  const cli = new PhuxCli({ runner: async (request) => {
    requests.push(request);
    if (request.args[0] !== "agent") throw new Error("expected agent command");
    if (request.args[1] === "set") {
      latestRecord = {
        name: option(request.args, "--name"),
        kind: option(request.args, "--kind"),
        state: option(request.args, "--state"),
        attention: option(request.args, "--attention"),
        session: option(request.args, "--session"),
      };
      return completed(`@10\t${JSON.stringify(latestRecord)}`);
    }
    if (request.args[1] === "show") {
      showCount += 1;
      if (showCount === 1) return completed("", 1);
      return completed(JSON.stringify({
        schema_version: 1,
        agents: [{
          terminal: "@10",
          session: "shared",
          window: "window-0",
          agent: { id: "declared", label: "opencode", kind: "declared" },
          state: "working",
          confidence: 1,
          attention: "normal",
          title: null,
          cwd: null,
          sources: [{ kind: "agent_record", signal: "declared", confidence: 1, observed: JSON.stringify(latestRecord) }],
          explanation: "declared record",
        }],
      }));
    }
    if (request.args[1] === "clear") return completed("@10\t-");
    throw new Error(`unexpected agent command: ${request.args.join(" ")}`);
  } });
  const hooks = await PhuxPlugin({}, {
    cli,
    env: { PHUX_TARGET: "@10" },
    onLifecycleError: (error) => errors.push(error),
  });

  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "first", status: { type: "busy" } },
  } });
  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "second", status: { type: "busy" } },
  } });
  await hooks.dispose();

  assert.equal(showCount, 2, "the second owned session is inspected after the first fails");
  assert.equal(requests.filter((request) => request.args[1] === "clear").length, 1);
  assert.equal(errors.length, 1);
  const requestCount = requests.length;
  await hooks.dispose();
  assert.equal(requests.length, requestCount, "consumed ownership entries are not retried");
});

test("retry and unrelated public events do not invent lifecycle transitions", async () => {
  let calls = 0;
  const cli = new PhuxCli({ runner: async () => {
    calls += 1;
    return completed();
  } });
  const hooks = await PhuxPlugin({}, { cli, env: { PHUX_TARGET: "@8" } });

  await hooks.event({ event: {
    type: "session.status",
    properties: { sessionID: "retrying", status: { type: "retry", attempt: 1, message: "later", next: 1 } },
  } });
  await hooks.event({ event: { type: "file.edited", properties: { file: "x" } } });
  await hooks.dispose();
  assert.equal(calls, 0);
});
