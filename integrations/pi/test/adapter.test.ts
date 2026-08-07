import assert from "node:assert/strict";
import test from "node:test";

import { PhuxCli } from "../src/adapter.js";
import { PhuxError } from "../src/errors.js";
import type { ProcessResult, ProcessRunner, RunRequest } from "../src/runner.js";

const completed = (stdout: string, exitCode = 0, stderr = ""): ProcessResult => ({
  termination: "completed",
  exitCode,
  stdout,
  stderr,
});

function fakeRunner(result: ProcessResult): { runner: ProcessRunner; requests: RunRequest[] } {
  const requests: RunRequest[] = [];
  return {
    requests,
    runner: async (request) => {
      requests.push(request);
      return result;
    },
  };
}

function expectCode(code: PhuxError["code"]): (error: unknown) => boolean {
  return (error) => error instanceof PhuxError && error.code === code;
}

const screenJson = JSON.stringify({
  schema_version: 3,
  pane: 1,
  cols: 80,
  rows: 1,
  cursor: { x: 0, y: 0, visible: true },
  lines: ["ready"],
  scrollback: [],
});

test("ls parses the documented CLI shape and passes flags before positionals", async () => {
  const fake = fakeRunner(completed(JSON.stringify({
    schema_version: 1,
    sessions: [{ name: "work", windows: 2, attached: false }],
  })));
  const cli = new PhuxCli({ runner: fake.runner, executable: "/opt/bin/phux", socket: "/tmp/p.sock" });

  const result = await cli.ls();

  assert.deepEqual(result.sessions, [{ name: "work", windows: 2, attached: false }]);
  assert.deepEqual(fake.requests[0]?.args, ["ls", "--json", "--socket", "/tmp/p.sock"]);
});

test("create uses documented new --json argv and parses the seed pane", async () => {
  const fake = fakeRunner(completed(JSON.stringify({ session: "work", terminal_id: 7 })));
  const cli = new PhuxCli({ runner: fake.runner, socket: "/tmp/p.sock" });

  const result = await cli.create("work", { cwd: "/repo", command: ["bash", "-lc", "echo ok"] });

  assert.deepEqual(result, { session: "work", terminal_id: 7 });
  assert.deepEqual(fake.requests[0]?.args, [
    "new", "--json", "-s", "work", "--cwd", "/repo", "--socket", "/tmp/p.sock",
    "--", "bash", "-lc", "echo ok",
  ]);
});

test("agentList inventories canonical panes and preserves owning sessions", async () => {
  const fake = fakeRunner(completed(JSON.stringify({
    schema_version: 1,
    agents: [{
      terminal: "@3",
      session: "work",
      window: "window-0",
      agent: { id: "codex", label: "Codex", kind: "codex" },
      state: "working",
      confidence: 0.9,
      attention: "normal",
      title: null,
      cwd: "/repo",
      sources: [],
      explanation: "working cue",
    }],
  })));
  const cli = new PhuxCli({ runner: fake.runner, socket: "/tmp/p.sock" });

  const result = await cli.agentList();

  assert.equal(result.agents[0]?.terminal, "@3");
  assert.equal(result.agents[0]?.session, "work");
  assert.deepEqual(fake.requests[0]?.args, ["agent", "list", "--json", "--socket", "/tmp/p.sock"]);
});

test("agent show/set/clear use documented commands and validate confirmations", async () => {
  const requests: RunRequest[] = [];
  const outputs = [
    completed(JSON.stringify({ schema_version: 1, agents: [] })),
    completed('@3\t{"name":"pi","kind":"pi","state":"working","attention":"normal","session":"pi:s-1"}\n'),
    completed("@3\t-\n"),
  ];
  const runner: ProcessRunner = async (request) => {
    requests.push(request);
    const result = outputs.shift();
    if (result === undefined) throw new Error("unexpected request");
    return result;
  };
  const cli = new PhuxCli({ runner, socket: "/tmp/p.sock" });
  const record = {
    name: "pi",
    kind: "pi",
    state: "working",
    attention: "normal",
    session: "pi:s-1",
  } as const;

  await cli.agentShow({ target: "@3" });
  assert.deepEqual(await cli.agentSet("@3", record), record);
  await cli.agentClear("@3");

  assert.deepEqual(requests.map((request) => request.args), [
    ["agent", "show", "--json", "--socket", "/tmp/p.sock", "@3"],
    ["agent", "set", "@3", "--name", "pi", "--kind", "pi", "--state", "working", "--attention", "normal", "--session", "pi:s-1", "--socket", "/tmp/p.sock"],
    ["agent", "clear", "@3", "--socket", "/tmp/p.sock"],
  ]);
});

test("agent set and clear reject unconfirmed responses", async () => {
  const set = new PhuxCli({ runner: fakeRunner(completed("not a confirmation")).runner });
  await assert.rejects(
    set.agentSet("@3", { name: "pi", kind: "pi", state: "idle", attention: "low", session: "pi:s" }),
    expectCode("invalid_response"),
  );
  const clear = new PhuxCli({ runner: fakeRunner(completed("@3\tstill-there")).runner });
  await assert.rejects(clear.agentClear("@3"), expectCode("invalid_response"));
});

test("wait preserves the final screen for exit 0 and specialized exit 124", async () => {
  const satisfied = new PhuxCli({ runner: fakeRunner(completed(screenJson, 0)).runner });
  assert.deepEqual(await satisfied.wait(), {
    outcome: "satisfied",
    screen: {
      schema_version: 3,
      pane: 1,
      cols: 80,
      rows: 1,
      cursor: { x: 0, y: 0, visible: true },
      lines: ["ready"],
      scrollback: [],
    },
  });

  const timedOut = new PhuxCli({ runner: fakeRunner(completed(screenJson, 124)).runner });
  const outcome = await timedOut.wait();
  assert.equal(outcome.outcome, "timed_out");
  assert.deepEqual(outcome.screen.lines, ["ready"]);
});

test("wait keeps unrelated nonzero exits as command failures", async () => {
  const cli = new PhuxCli({ runner: fakeRunner(completed("", 1, "no server")).runner });
  await assert.rejects(cli.wait(), expectCode("command_failed"));
});

test("run treats a documented nonzero child exit as typed data", async () => {
  const fake = fakeRunner(completed(JSON.stringify({
    command: "false",
    exit_code: 7,
    output: "failure",
    duration_ms: 12,
    truncated: false,
  }), 7));
  const cli = new PhuxCli({ runner: fake.runner, socket: "/tmp/p.sock" });

  const result = await cli.run("work", ["false"], { phuxTimeoutSeconds: 30 });

  assert.equal(result.exit_code, 7);
  assert.deepEqual(fake.requests[0]?.args, [
    "run", "--json", "--timeout", "30", "--socket", "/tmp/p.sock", "work", "false",
  ]);
});

test("orchestration commands use exact canonical argv and typed responses", async () => {
  const requests: RunRequest[] = [];
  const outputs: ProcessResult[] = [
    completed(JSON.stringify({ terminal_id: 8, satellite: null })),
    completed(JSON.stringify({ schema_version: 1, terminal_id: 9, integration: "codex", plugin: "agents", argv: ["not-exposed"] })),
    completed(""), completed("phux: signalled @8 (Freeze)\n"),
    completed("@8\tbuild ci\n"),
    completed(JSON.stringify({ event: "asked", terminal: "@8", id: "q", question: "Approve?", suggestions: ["yes"], elapsed_seconds: 3 })),
  ];
  const runner: ProcessRunner = async (request) => {
    requests.push(request);
    const result = outputs.shift();
    if (result === undefined) throw new Error("unexpected request");
    return result;
  };
  const cli = new PhuxCli({ runner, socket: "/tmp/p.sock" });

  assert.deepEqual(await cli.spawn({ cwd: "/repo", command: ["bash"] }), { terminal_id: 8, satellite: null });
  assert.equal((await cli.launch("codex", { extra: ["--model", "x"] })).terminal_id, 9);
  await cli.kill("@8");
  await cli.signal("@8", "freeze");
  assert.deepEqual(await cli.tag("add", "@8", ["build", "ci"]), [{ terminal: "@8", tagsText: "build ci" }]);
  assert.equal((await cli.ask("@8", "Approve?", { id: "q", suggestions: ["yes"], elapsedSeconds: 3 })).event, "asked");

  assert.deepEqual(requests.map((request) => request.args), [
    ["spawn", "--json", "--cwd", "/repo", "--socket", "/tmp/p.sock", "--", "bash"],
    ["launch", "--json", "--socket", "/tmp/p.sock", "codex", "--", "--model", "x"],
    ["kill", "@8", "--socket", "/tmp/p.sock"],
    ["signal", "@8", "freeze", "--socket", "/tmp/p.sock"],
    ["tag", "add", "@8", "build", "ci", "--socket", "/tmp/p.sock"],
    ["ask", "@8", "--json", "--id", "q", "--suggest", "yes", "--elapsed-seconds", "3", "--socket", "/tmp/p.sock", "Approve?"],
  ]);
});

test("placement argv is canonical and satellite placement is rejected before execution", async () => {
  const requests: RunRequest[] = [];
  const cli = new PhuxCli({
    executable: "/opt/phux",
    socket: "/tmp/shared.sock",
    runner: async (request) => {
      requests.push(request);
      if (request.args[0] === "spawn") return completed(JSON.stringify({ terminal_id: 8, satellite: null }));
      return completed(JSON.stringify({
        schema_version: 1, terminal_id: 9, integration: "codex", plugin: "agents", argv: ["private"],
      }));
    },
  });

  await cli.spawn({ target: "@3", split: "vertical", ratio: 0.4, cwd: "/repo" });
  await cli.launch("codex", { target: "work:0.0", split: "horizontal", ratio: 0.6 });

  assert.deepEqual(requests.map((request) => request.args), [
    ["spawn", "--json", "--target", "@3", "--split", "vertical", "--ratio", "0.4", "--cwd", "/repo", "--socket", "/tmp/shared.sock"],
    ["launch", "--json", "--target", "work:0.0", "--split", "horizontal", "--ratio", "0.6", "--socket", "/tmp/shared.sock", "codex"],
  ]);
  await assert.rejects(cli.spawn({ split: "vertical" }), /target is required/);
  await assert.rejects(cli.spawn({ satellite: "edge", target: "@3" }), /cannot be combined/);
  await assert.rejects(cli.launch("codex", { target: "edge\/@7" }), /local-only/);
  assert.equal(requests.length, 2);
});

test("spatial methods emit exact canonical argv and parse versioned results", async () => {
  const requests: RunRequest[] = [];
  const cli = new PhuxCli({
    socket: "/tmp/shared.sock",
    runner: async (request) => {
      requests.push(request);
      switch (request.args[0]) {
        case "insert-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "insert-pane", session_id: 5,
          target_terminal_id: 3, new_terminal_id: 4, direction: "vertical", ratio: 0.4,
        }));
        case "move-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "move-pane", session_id: 5,
          source_terminal_id: 4, target_terminal_id: 3, direction: "horizontal", ratio: 0.6,
        }));
        case "swap-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "swap-pane", session_id: 5,
          first_terminal_id: 3, second_terminal_id: 4,
        }));
        default: throw new Error("unexpected request");
      }
    },
  });

  assert.equal((await cli.insertPane("@3", "@4", { direction: "vertical", ratio: 0.4 })).operation, "insert-pane");
  assert.equal((await cli.movePane("@4", "@3", { direction: "horizontal", ratio: 0.6 })).operation, "move-pane");
  assert.equal((await cli.swapPane("@3", "@4")).operation, "swap-pane");

  assert.deepEqual(requests.map((request) => request.args), [
    ["insert-pane", "--json", "--split", "vertical", "--ratio", "0.4", "--socket", "/tmp/shared.sock", "@3", "@4"],
    ["move-pane", "--json", "--split", "horizontal", "--ratio", "0.6", "--socket", "/tmp/shared.sock", "@4", "@3"],
    ["swap-pane", "--json", "--socket", "/tmp/shared.sock", "@3", "@4"],
  ]);
  await assert.rejects(cli.swapPane("@3", "@3"), /distinct/);
  await assert.rejects(cli.insertPane("edge\/@3", "@4"), /local/);
  assert.equal(requests.length, 3);
});

test("watch turns the streaming CLI into a bounded typed event collection", async () => {
  const fake = fakeRunner({
    termination: "timed_out", exitCode: null, stderr: "",
    stdout: [
      JSON.stringify({ event: "dirty", terminal: "@3" }),
      JSON.stringify({ event: "asked", terminal: "@3", id: "q", question: "Help?", suggestions: [], elapsed_seconds: null }),
    ].join("\n"),
  });
  const cli = new PhuxCli({ runner: fake.runner, socket: "/tmp/p.sock" });

  const result = await cli.watch({ target: "@3", durationMs: 250, maxEvents: 1 });

  assert.deepEqual(result.events.map((event) => event.event), ["asked"]);
  assert.equal(result.truncated, true);
  assert.equal(result.ended, false);
  assert.equal(fake.requests[0]?.timeoutMs, 250);
  assert.deepEqual(fake.requests[0]?.args, ["watch", "--json", "--socket", "/tmp/p.sock", "@3"]);
});

test("rendered snapshot validates its dense versioned frame", async () => {
  const style = {
    bold: false, faint: false, italic: false, underline: false, blink: false,
    inverse: false, invisible: false, strikethrough: false, overline: false,
    fg: { kind: "default" }, bg: { kind: "default" },
  };
  const fake = fakeRunner(completed(JSON.stringify({
    schema_version: 1, cols: 2, rows: 1, cursor: null,
    cells: [{ grapheme: "a", style }, { grapheme: " ", style }],
  })));
  const cli = new PhuxCli({ runner: fake.runner, socket: "/tmp/p.sock" });

  assert.equal((await cli.renderedSnapshot({ session: "work", cols: 2, rows: 1 })).cells.length, 2);
  assert.deepEqual(fake.requests[0]?.args, [
    "snapshot", "--rendered", "--json", "--cols", "2", "--rows", "1", "--socket", "/tmp/p.sock", "work",
  ]);
});

test("malformed JSON is normalized", async () => {
  const cli = new PhuxCli({ runner: fakeRunner(completed("not-json")).runner });
  await assert.rejects(cli.ls(), expectCode("malformed_json"));
});

test("run wrapper failures without JSON stay command failures", async () => {
  const cli = new PhuxCli({ runner: fakeRunner(completed("", 125, "sentinel timed out")).runner });
  await assert.rejects(cli.run("work", ["sleep", "10"]), expectCode("command_failed"));
});

test("ordinary nonzero exits include the phux diagnostic", async () => {
  const cli = new PhuxCli({ runner: fakeRunner(completed("", 1, "no server running")).runner });
  await assert.rejects(
    cli.ls(),
    (error) => error instanceof PhuxError &&
      error.code === "command_failed" &&
      error.exitCode === 1 &&
      error.message.includes("no server running"),
  );
});

test("abort and local timeout are distinct normalized errors", async () => {
  const aborted = new PhuxCli({
    runner: fakeRunner({ termination: "aborted", exitCode: null, stdout: "", stderr: "" }).runner,
  });
  const timedOut = new PhuxCli({
    runner: fakeRunner({ termination: "timed_out", exitCode: null, stdout: "", stderr: "" }).runner,
  });

  await assert.rejects(aborted.ls(), expectCode("aborted"));
  await assert.rejects(timedOut.ls(), expectCode("timeout"));
});

test("output overflow is exposed as a typed actionable error", async () => {
  const overflow: ProcessResult = {
    termination: "output_limit",
    outputLimit: "stdout",
    exitCode: null,
    stdout: "partial",
    stderr: "",
  };
  const cli = new PhuxCli({ runner: fakeRunner(overflow).runner, maxStdoutBytes: 7 });
  await assert.rejects(
    cli.ls(),
    (error) => error instanceof PhuxError &&
      error.code === "output_limit" &&
      error.message.includes("7-byte stdout"),
  );
});

test("CLI parsers reject similar MCP response shapes", async () => {
  const mcpLs = new PhuxCli({ runner: fakeRunner(completed(JSON.stringify({
    schema_version: 1,
    sessions: [{ name: "work", window_count: 2, attached_client_count: 0 }],
  }))).runner });
  await assert.rejects(mcpLs.ls(), expectCode("invalid_response"));

  const mcpRun = new PhuxCli({ runner: fakeRunner(completed(JSON.stringify({
    outcome: "timed_out",
    command: "sleep 10",
    duration_ms: 1_000,
  }), 125)).runner });
  await assert.rejects(mcpRun.run("work", ["sleep", "10"]), expectCode("invalid_response"));
});

test("probe reports compatible versions and rejects versions below the package minimum", async () => {
  const present = new PhuxCli({ runner: fakeRunner(completed("phux 0.1.0\n")).runner });
  assert.deepEqual(await present.probe(), {
    available: true,
    version: "0.1.0",
    rawVersion: "phux 0.1.0",
  });

  for (const version of ["0.0.99", "0.1.0-alpha.1"]) {
    const old = new PhuxCli({ runner: fakeRunner(completed(`phux ${version}\n`)).runner });
    assert.deepEqual(await old.probe(), {
      available: false,
      version,
      rawVersion: `phux ${version}`,
      reason: `@phux/pi requires phux >= 0.1.0; found ${version}`,
    });
  }

  const missing: ProcessRunner = async () => {
    const error = Object.assign(new Error("spawn phux ENOENT"), { code: "ENOENT" });
    throw error;
  };
  const result = await new PhuxCli({ runner: missing }).probe();
  assert.equal(result.available, false);
  assert.match(result.reason ?? "", /install phux/);
});
