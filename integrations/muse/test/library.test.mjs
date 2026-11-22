import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const library = fileURLToPath(new URL("../../../.agents/lib/phux-agent.sh", import.meta.url));
const smoke = fileURLToPath(new URL("../scripts/smoke.sh", import.meta.url));
const defaults = {
  status: { stdout: { running: true } },
  new: { stdout: { session: "scratch", terminal_id: 42 } },
  run: { stdout: { output: "hello", exit_code: 0 } },
  snapshot: { stdout: { lines: ["hello", "world"] } },
  ls: { stdout: { sessions: [{ name: "scratch" }] } },
  wait: { stdout: {} },
  kill: { stdout: {} },
};

// PATH contains only a fake phux before the normal runtime tools. No test can
// reach the installed binary, server or inherited production socket.
function drive(script, responses = {}, env = {}, shell = "/bin/sh") {
  const dir = mkdtempSync(join(tmpdir(), "phux-muse-unit-"));
  try {
    const log = join(dir, "calls");
    writeFileSync(join(dir, "phux"), `#!/usr/bin/env node
import { appendFileSync } from 'node:fs';
const args = process.argv.slice(2);
appendFileSync(process.env.MOCK_LOG, JSON.stringify(args) + '\\n');
const result = JSON.parse(process.env.MOCK_RESPONSES)[args[0]];
if (!result) { console.error('unexpected CLI call'); process.exit(99); }
process.stdout.write(typeof result.stdout === 'string' ? result.stdout : JSON.stringify(result.stdout));
process.exit(result.code ?? 0);
`, { mode: 0o755 });
    const result = spawnSync(shell, ["-c", '. "$LIBRARY"\n' + script], {
      encoding: "utf8",
      timeout: 10_000,
      env: {
        ...process.env,
        PATH: `${dir}:${process.env.PATH}`,
        LIBRARY: library,
        PHUX_SOCKET: join(dir, "mock.sock"),
        PHUX_TERMINAL_ID: "",
        PHUX_AGENT_TIMEOUT: "20",
        MOCK_LOG: log,
        MOCK_RESPONSES: JSON.stringify({ ...defaults, ...responses }),
        ...env,
      },
    });
    let calls = [];
    try { calls = readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map(JSON.parse); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    assert.ifError(result.error);
    return { ...result, calls };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("spawn returns an exact pane, quotes the name, and requests a POSIX shell", () => {
  const name = "scratch'$(touch NEVER) name";
  const result = drive('phux_spawn "$NAME"', {}, { NAME: name });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "@42\n");
  assert.deepEqual(result.calls, [["new", "--json", "-s", name, "--ephemeral", "--", "/bin/sh"]]);
});

test("failed CLI calls are not hidden by JSON parsing", () => {
  for (const [verb, script] of [
    ["new", "phux_spawn scratch"], ["snapshot", "phux_snap @42"],
    ["ls", "phux_ls"], ["kill", "phux_kill @42"], ["wait", "phux_wait @42 text"],
  ]) {
    const result = drive(script, { [verb]: { stdout: { error: { code: "denied" } }, code: 7 } });
    assert.equal(result.status, 7, `${verb}: ${result.stderr}`);
    assert.equal(result.stdout, "");
    assert.match(result.stderr, /denied/);
  }
});

test("run preserves failed child output, command quoting and exit status", () => {
  const command = 'printf "%s" "a b"; false';
  const result = drive('phux_run @42 "$COMMAND" 9', {
    run: { stdout: { output: "failure output", exit_code: 23 }, code: 23 },
  }, { COMMAND: command });
  assert.equal(result.status, 23);
  assert.equal(result.stdout, "failure output\n");
  assert.deepEqual(result.calls, [["run", "--json", "--timeout", "9", "@42", command]]);
});

test("run/wait timeouts and unknown delivery are retained without retry", () => {
  for (const [verb, script, code] of [["run", "phux_run @42 cmd", 125], ["wait", "phux_wait @42 text", 124]]) {
    const result = drive(script, { [verb]: { stdout: { error: { code: "delivery_unknown" } }, code } });
    assert.equal(result.status, code);
    assert.equal(result.calls.length, 1);
    assert.match(result.stderr, /delivery_unknown/);
  }
});

test("probes never auto-start a server, including malformed/failed status", () => {
  for (const response of [{ stdout: { running: false }, code: 1 }, { stdout: "bad json" }, { stdout: {} }]) {
    const result = drive("phux_init", { status: response });
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, [["status", "--json"]]);
  }
  const result = drive("phux_snap @42; phux_ls; phux_kill @42");
  assert.deepEqual(result.calls.map(([verb]) => verb), ["snapshot", "ls", "kill"]);
});

test("malformed and missing result fields fail, but empty lists/screens succeed", () => {
  for (const [verb, script] of [["new", "phux_spawn scratch"], ["snapshot", "phux_snap @42"], ["ls", "phux_ls"], ["run", "phux_run @42 cmd"]]) {
    for (const stdout of ["bad json", {}, { error: { code: "broken" } }]) {
      assert.notEqual(drive(script, { [verb]: { stdout } }).status, 0, verb);
    }
  }
  assert.equal(drive("phux_ls", { ls: { stdout: { sessions: [] } } }).status, 0);
  assert.equal(drive("phux_snap @42", { snapshot: { stdout: { lines: [] } } }).status, 0);
});

test("unsafe selectors, the hosting pane and unbounded/invalid timeouts are refused before CLI calls", () => {
  for (const target of [".", "=", "scratch", "#tag", "@", "@0", "@042", "@1;evil", "host/@1", ""]) {
    for (const fn of ["phux_run", "phux_snap", "phux_wait", "phux_kill"]) {
      const result = drive(`${fn} "$TARGET"${fn === "phux_run" || fn === "phux_wait" ? ' text' : ''}`, {}, { TARGET: target });
      assert.equal(result.status, 2, `${fn} ${target}`);
      assert.deepEqual(result.calls, []);
    }
  }
  for (const self of ["42", "@42", "malformed", "@", "@042", "host/@42"]) {
    assert.deepEqual(drive("phux_run @42 cmd", {}, { PHUX_TERMINAL_ID: self }).calls, []);
    assert.deepEqual(drive("phux_kill @42", {}, { PHUX_TERMINAL_ID: self }).calls, []);
  }
  for (const timeout of ["0", "-1", "forever", "1.5", "00"]) {
    for (const fn of ["phux_run @42 cmd", "phux_wait @42 text", "phux_do scratch cmd"]) {
      const result = drive(`${fn} "$TIMEOUT"`, {}, { TIMEOUT: timeout });
      assert.equal(result.status, 2);
      assert.deepEqual(result.calls, []);
    }
  }
});

test("one-shot cleanup happens exactly once on success, child/snapshot failure and cleanup failure", () => {
  for (const [responses, status] of [
    [{}, 0],
    [{ run: { stdout: { output: "bad", exit_code: 23 }, code: 23 } }, 23],
    [{ snapshot: { stdout: {}, code: 8 } }, 8],
    [{ kill: { stdout: {}, code: 9 } }, 1],
  ]) {
    const result = drive("set -eu; phux_do scratch cmd", responses);
    assert.equal(result.status, status, result.stderr);
    assert.equal(result.calls.filter(([verb]) => verb === "kill").length, 1);
    assert.deepEqual(result.calls.at(-1), ["kill", "--yes", "@42"]);
  }
  const result = drive("phux_do scratch cmd", { new: { stdout: {}, code: 5 } });
  assert.equal(result.status, 5);
  assert.deepEqual(result.calls.map(([verb]) => verb), ["new"]);
});

test("caller variables, options and traps survive helper calls (POSIX sh and bash)", () => {
  for (const shell of ["/bin/sh", "/bin/bash"]) {
    const result = drive(`set -u
out=caller; code=caller; pane=caller; timeout=caller
trap 'printf "caller exit\\n"' 0
trap 'printf "caller term\\n"' TERM
before=$(trap)
phux_do scratch cmd >/dev/null
[ "$out:$code:$pane:$timeout" = caller:caller:caller:caller ] || exit 77
[ "$before" = "$(trap)" ] || exit 78
case "$-" in *u*) ;; *) exit 79 ;; esac`, {}, {}, shell);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, "caller exit\n");
  }
});

test("handled INT/TERM clean up the owned pane and retain signal status", () => {
  for (const [signal, status] of [["INT", 130], ["TERM", 143]]) {
    const result = drive(`phux_run() { kill -${signal} "$(exec sh -c 'echo "$PPID"')"; }
phux_do scratch cmd`, {}, {}, "/bin/bash");
    assert.equal(result.status, status, result.stderr);
    assert.deepEqual(result.calls.map(([verb]) => verb), ["new", "kill"]);
    assert.deepEqual(result.calls.at(-1), ["kill", "--yes", "@42"]);
  }
});

test("smoke refuses to use an inherited server socket without explicit opt-in", () => {
  const result = drive('sh "$SMOKE_SCRIPT"', {}, { SMOKE_SCRIPT: smoke });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /isolated-test.sock/);
  assert.deepEqual(result.calls, []);
});
