// Regression checks for fail-closed browser execution and bounded cleanup.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { createSocket } from "node:dgram";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  boundPort,
  isolatedEnvironment,
  poll,
  REQUIRED_TESTS,
  requireBrowserTests,
  run,
  scratchBase,
  signalGroup,
  socketPathMax,
  udpBlackhole,
  webdriverCapabilities,
  websocketReady,
} from "./browser.ts";
import { AgentFixture } from "./fixture.ts";

let scratch: string;
beforeEach(() => {
  scratch = mkdtempSync(join(tmpdir(), "phux-web-browser-test-"));
});
afterEach(() => {
  rmSync(scratch, { recursive: true, force: true });
});

const running = { exitCode: null, signalCode: null };
const exited = { exitCode: 1, signalCode: null };
const noPause = () => Promise.resolve();
const PATH = process.env["PATH"] ?? "/usr/bin:/bin";

/** The message a promise rejects with; resolving is a test failure. */
async function rejection(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected a rejection");
}

describe("AgentFixture", () => {
  test("is canned, token-gated, targets its own socket, and cleans up", async () => {
    const phux = join(scratch, "phux");
    writeFileSync(phux, `#!/bin/sh\nprintf '%s\\n' "$*" >> "$0.calls"\n`, { mode: 0o755 });
    const calls = () =>
      existsSync(`${phux}.calls`) ? readFileSync(`${phux}.calls`, "utf8").trim().split("\n") : [];
    const fixture = new AgentFixture(phux, "/tmp/owned.sock", {});
    const post = (url: string) => fetch(url, { method: "POST" });
    try {
      expect((await post(`${fixture.url.replace(fixture.token, "wrong")}/open`)).status).toBe(404);
      expect(calls()).toEqual([]);
      const open = await post(`${fixture.url}/open`);
      expect(open.status).toBe(200);
      expect(open.headers.get("Access-Control-Allow-Origin")).toBe("*");
      expect(fixture.opened).toBe(true);
      expect((await post(`${fixture.url}/arbitrary-command`)).status).toBe(500);
    } finally {
      await fixture.close();
    }
    expect(calls()[0]).toBe("agent session open default --provider pi --socket /tmp/owned.sock");
    expect(calls().at(-1)).toBe("agent session close default --socket /tmp/owned.sock");
    expect(calls().every((call) => call.endsWith(" --socket /tmp/owned.sock"))).toBe(true);
    await rejection(post(`${fixture.url}/open`));
  });

  test("a failing CLI command is a 500, not a crash", async () => {
    const fixture = new AgentFixture("/usr/bin/false", "/tmp/owned.sock", {});
    try {
      expect((await fetch(`${fixture.url}/open`, { method: "POST" })).status).toBe(500);
      expect(fixture.opened).toBe(false);
    } finally {
      await fixture.close();
    }
  });
});

test("the UDP blackhole counts the datagrams it discards", async () => {
  const blackhole = await udpBlackhole();
  try {
    const sender = createSocket("udp4");
    await new Promise<void>((done, fail) =>
      sender.send("quic", blackhole.port, "127.0.0.1", (error) => (error ? fail(error) : done())),
    );
    sender.close();
    for (let attempt = 0; attempt < 100 && blackhole.packets() === 0; attempt += 1) {
      await Bun.sleep(10);
    }
    expect(blackhole.packets()).toBeGreaterThan(0);
  } finally {
    blackhole.close();
  }
});

test.each([
  [undefined, false, {}],
  ["/selected/chromium", false, { "goog:chromeOptions": { binary: "/selected/chromium" } }],
  [undefined, true, { acceptInsecureCerts: true }],
])("webdriver capabilities for Chrome %p, insecure %p", (chrome, insecure, expected) => {
  const path = webdriverCapabilities(scratch, chrome, insecure);
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual(expected);
});

describe("owned process groups", () => {
  async function assertStopped(pid: number): Promise<void> {
    // Linux init may briefly retain an orphan zombie; it is no longer running.
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const result = Bun.spawnSync(["ps", "-p", String(pid), "-o", "stat="], { stdout: "pipe" });
      const state = result.stdout.toString().trim();
      if (!state || state.startsWith("Z")) return;
      await Bun.sleep(10);
    }
    throw new Error(`SIGINT-ignoring descendant ${pid} survived owned-group cleanup`);
  }

  // The descendant ignores SIGINT, so the SIGKILL escalation is under test.
  test.each([
    ["a timeout", false],
    ["an exited leader", true],
  ])("%s still stops a SIGINT-ignoring descendant", async (_, parentExits) => {
    // Single-quoted, so `$$` is the descendant's own pid.
    const child = `'trap "" INT; echo $$ > child; exec sleep 60'`;
    const parent =
      `echo $$ > group; sh -c ${child} & ` +
      `while [ ! -s child ]; do sleep 0.01; done; sleep ${parentExits ? 0 : 60}`;
    const timeoutMs = parentExits ? 5000 : 1000;
    const logPath = join(scratch, "run.log");
    try {
      const result = run(["sh", "-c", parent], {
        cwd: scratch,
        env: { PATH },
        logPath,
        graceMs: 500,
        timeoutMs,
      });
      if (parentExits) await result;
      else expect(await rejection(result)).toContain("timed out after 1000ms");
      await assertStopped(Number(readFileSync(join(scratch, "child"), "utf8")));
    } finally {
      // Clean up even when run() regresses: only this fixture's own group.
      const group = join(scratch, "group");
      if (existsSync(group)) signalGroup(Number(readFileSync(group, "utf8")), "SIGKILL");
    }
  });

  test("a command failure is not masked", async () => {
    const options = { cwd: scratch, env: {}, logPath: join(scratch, "run.log"), timeoutMs: 5000 };
    expect(await rejection(run(["sh", "-c", "exit 7"], options))).toContain("exited 7");
  });

  test("a group that no longer exists is not signalable", async () => {
    const child = Bun.spawn(["true"], { detached: true });
    await child.exited;
    expect(signalGroup(child.pid, 0)).toBe(false);
  });
});

test("every required browser test must pass, module-qualified or not", () => {
  const passing = REQUIRED_TESTS.map((name) => `test ${name} ... ok`).join("\n");
  const verify = (output: string) => () => requireBrowserTests(output, REQUIRED_TESTS);
  expect(verify(passing)()).toContain(`${REQUIRED_TESTS.length} required`);
  expect(verify(passing.replace("synthesized_only", "skipped_only"))).toThrow("synthesized_only");
  expect(verify("no tests to run!\ntest result: ok. 0 passed")).toThrow("renders_engine_grid");
  expect(requireBrowserTests("test e2e::blackholed ... ok", ["blackholed"])).toContain(
    "1 required",
  );
});

describe("demo server readiness", () => {
  test("polling is bounded, stops at the first value, and fails on an early exit", async () => {
    let probes = 0;
    const refuse = () => {
      probes += 1;
      return Promise.reject(new Error("connection refused"));
    };
    expect(await rejection(poll(running, "readiness", 120, refuse, noPause))).toContain(
      "120 bounded probes",
    );
    expect(probes).toBe(120);
    const answers = [undefined, 7];
    expect(await poll(running, "readiness", 120, async () => answers.shift(), noPause)).toBe(7);
    expect(answers).toEqual([]);
    expect(await rejection(poll(exited, "readiness", 120, async () => 1))).toContain(
      "exited before readiness",
    );
  });

  test("the bound port is read from the listening line", async () => {
    const log = join(scratch, "server.log");
    writeFileSync(log, "ws-demo-server listening on wss://127.0.0.1:43127/  (seed: default)\n");
    expect(await boundPort(running, log)).toBe(43127);
  });

  test("a real upgrade is ready and a plain HTTP answer is not", async () => {
    const server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch: (request, server) =>
        server.upgrade(request) ? undefined : new Response("no", { status: 400 }),
      websocket: { message() {} },
    });
    const plain = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: () => new Response("no") });
    try {
      expect(await websocketReady(server.port ?? 0)).toBe(true);
      expect(await websocketReady(plain.port ?? 0)).toBe(false);
    } finally {
      await server.stop(true);
      await plain.stop(true);
    }
  });
});

test("inherited phux configuration never reaches the demo server", () => {
  const env = isolatedEnvironment({
    PATH: "/bin",
    TMPDIR: "/tmp",
    PHUX_SOCKET: "/tmp/phux-user/phux.sock",
    PHUX_QUIC_ADDR: "0.0.0.0:8788",
    PHUX_WS_TOKENS: "/home/user/.local/state/phux/remote-tokens",
    PHUX_WS_TLS_CERT: "/home/user/.local/state/phux/remote-cert.pem",
  });
  expect(env).toEqual({ PATH: "/bin", TMPDIR: "/tmp" });
});

describe("browser scratch", () => {
  // Literal, not the runner's constant: a shorter budget must not pass.
  const nested = (base: string) =>
    Buffer.byteLength(`${base}/phux-browser-XXXXXXXX/mbx-session-XXXXXXXX/cache-agent.sock`);
  const macTmp = "/var/folders/3m/8h8q2n1d4c5b6a7z8y9x0w1v2u3t4s/T";
  // One byte past Darwin's limit, which still fits Linux's.
  let linuxOnly = "/var/tmpdirs";
  while (nested(linuxOnly) <= socketPathMax("darwin")) linuxOnly += "p";

  test("the boundary cases sit where the cases claim", () => {
    expect(nested(macTmp)).toBeGreaterThan(socketPathMax("darwin"));
    expect(nested(linuxOnly)).toBeLessThanOrEqual(socketPathMax("linux"));
  });

  test.each([
    ["a long macOS TMPDIR moves to /tmp", macTmp, "darwin", "/tmp"],
    ["Linux keeps a TMPDIR within its own limit", linuxOnly, "linux", linuxOnly],
    ["Darwin moves the same TMPDIR", linuxOnly, "darwin", "/tmp"],
    ["a short TMPDIR is kept", "/tmp/phux-caller", "darwin", "/tmp/phux-caller"],
  ])("%s", (_, tmp, platform, expected) => {
    expect(scratchBase(tmp, platform)).toBe(expected);
  });

  test("no short scratch base names the socket limit", () => {
    const longTmp = `/var/folders/xx/${"y".repeat(30)}/T`;
    const choose = () => scratchBase(longTmp, "darwin", ["/no/such/phux-short-base"]);
    for (const part of ["TMPDIR", longTmp, "103", "mbx-session-XXXXXXXX/cache-agent.sock"]) {
      expect(choose).toThrow(part);
    }
  });
});
