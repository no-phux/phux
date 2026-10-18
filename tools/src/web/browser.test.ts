// Regression checks for fail-closed browser execution and bounded cleanup.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  closeSync,
  existsSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createSocket } from "node:dgram";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import {
  boundPort,
  browserScratchBase,
  browserScratchDir,
  CommandFailed,
  CommandTimeout,
  DARWIN_SOCKET_PATH_MAX,
  harnessBuildEnv,
  isolatedEnvironment,
  LINUX_SOCKET_PATH_MAX,
  NESTED_SOCKET_SUFFIXES,
  REQUIRED_TESTS,
  requireBrowserTests,
  run,
  runChrome,
  signalGroup,
  socketPathMax,
  stop,
  TEMP_NAME_RANDOM,
  UdpBlackhole,
  waitReady,
  webdriverEnvironment,
  websocketReady,
} from "./browser.ts";
import { AgentFixture, type Exec } from "./fixture.ts";

let scratch: string;
beforeEach(() => {
  scratch = mkdtempSync(join(tmpdir(), "phux-web-browser-test-"));
});
afterEach(() => {
  rmSync(scratch, { recursive: true, force: true });
});

const noPause = () => Promise.resolve();

function readJson(path: string | undefined): Record<string, unknown> {
  if (path === undefined) throw new Error("no capabilities path");
  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (typeof parsed !== "object" || parsed === null) throw new Error("not an object");
  return Object.fromEntries(Object.entries(parsed));
}

/** The message a promise rejects with, and the error itself. */
async function failure(promise: Promise<unknown>): Promise<Error> {
  try {
    await promise;
  } catch (error) {
    if (error instanceof Error) return error;
    throw error;
  }
  throw new Error("expected a rejection");
}

test("the harness build opts out of the dev opt-level", () => {
  const base = { CARGO_TARGET_DIR: "/tmp/web-native", PATH: "/usr/bin" };
  const build = harnessBuildEnv(base);
  expect(build["CARGO_PROFILE_DEV_OPT_LEVEL"]).toBe("0");
  expect(build["CARGO_TARGET_DIR"]).toBe("/tmp/web-native");
  expect("CARGO_PROFILE_DEV_OPT_LEVEL" in base).toBe(false);
});

describe("AgentFixture", () => {
  test("is canned, token-gated, and cleans up", async () => {
    const calls: string[][] = [];
    const exec: Exec = (argv) => {
      calls.push(argv.slice(1, -2));
      return Promise.resolve();
    };
    const fixture = new AgentFixture("/test/phux", "/tmp/owned-test.sock", {}, exec).start();
    const url = fixture.url;
    try {
      const wrong = await fetch(`${url.replace(fixture.token, "wrong")}/open`, { method: "POST" });
      expect(wrong.status).toBe(404);
      expect(calls).toEqual([]);
      const open = await fetch(`${url}/open`, { method: "POST" });
      expect(open.status).toBe(200);
      expect(open.headers.get("Access-Control-Allow-Origin")).toBe("*");
      expect(fixture.opened).toBe(true);
      expect((await fetch(`${url}/arbitrary-command`, { method: "POST" })).status).toBe(500);
    } finally {
      await fixture.close();
    }
    expect(calls).toContainEqual(["agent", "session", "open", "default", "--provider", "pi"]);
    expect(calls.at(-1)).toEqual(["agent", "session", "close", "default"]);
    expect(fixture.serving).toBe(false);
    expect(await failure(fetch(`${url}/open`, { method: "POST" }))).toBeInstanceOf(Error);
  });

  test("its CLI always targets its owned socket", async () => {
    const seen: Parameters<Exec>[] = [];
    const exec: Exec = (...args) => {
      seen.push(args);
      return Promise.resolve();
    };
    const fixture = new AgentFixture("/test/phux", "/tmp/owned-test.sock", { PATH: "/bin" }, exec);
    await fixture.command("agent", "session", "close", "default");
    expect(seen).toEqual([
      [
        ["/test/phux", "agent", "session", "close", "default", "--socket", "/tmp/owned-test.sock"],
        { PATH: "/bin" },
        10_000,
      ],
    ]);
  });

  test("a failing CLI command is a 500, not a crash", async () => {
    const fixture = new AgentFixture("/usr/bin/false", "/tmp/owned-test.sock", {}).start();
    try {
      expect((await fetch(`${fixture.url}/open`, { method: "POST" })).status).toBe(500);
      expect(fixture.opened).toBe(false);
    } finally {
      await fixture.close();
    }
  });
});

test("the UDP blackhole receives and cleans up its owned socket", async () => {
  const blackhole = await new UdpBlackhole().start();
  try {
    const sender = createSocket("udp4");
    await new Promise<void>((done, fail) =>
      sender.send("quic", blackhole.port, "127.0.0.1", (error) => (error ? fail(error) : done())),
    );
    sender.close();
    for (let attempt = 0; attempt < 100 && blackhole.packets === 0; attempt += 1) {
      await Bun.sleep(10);
    }
    expect(blackhole.packets).toBeGreaterThan(0);
  } finally {
    blackhole.close();
  }
  expect(blackhole.open).toBe(false);
});

describe("webdriver capabilities", () => {
  test("the Chrome binary is in the runner's capabilities", () => {
    const env = webdriverEnvironment({ CHROME: "/selected/chromium" }, scratch, scratch);
    const capabilities = readJson(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]);
    expect(capabilities["goog:chromeOptions"]).toEqual({ binary: "/selected/chromium" });
  });

  test("the fixture accepts insecure certificates only in temporary capabilities", () => {
    const env = webdriverEnvironment({ PHUX_TEST_ACCEPT_INSECURE_CERTS: "1" }, scratch, scratch);
    expect(readJson(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"])["acceptInsecureCerts"]).toBe(true);
  });

  test("user capabilities survive binary selection", () => {
    const source = join(scratch, "custom.json");
    const original = {
      acceptInsecureCerts: true,
      "goog:chromeOptions": { args: ["--disable-gpu"], binary: "/old/chrome" },
    };
    writeFileSync(source, JSON.stringify(original));
    const output = mkdtempSync(join(scratch, "out-"));
    const env = webdriverEnvironment(
      { CHROME: "/selected/chromium", WASM_BINDGEN_TEST_WEBDRIVER_JSON: "custom.json" },
      output,
      scratch,
    );
    const capabilities = readJson(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"]);
    expect(capabilities["acceptInsecureCerts"]).toBe(true);
    expect(capabilities["goog:chromeOptions"]).toEqual({
      args: ["--disable-gpu"],
      binary: "/selected/chromium",
    });
    expect(readJson(source)).toEqual(original);
  });

  test("default capabilities are preserved without a Chrome override", () => {
    const source = join(scratch, "webdriver.json");
    const original = { "goog:chromeOptions": { binary: "/user/chrome", args: ["--disable-gpu"] } };
    writeFileSync(source, JSON.stringify(original));
    const output = mkdtempSync(join(scratch, "out-"));
    const env = webdriverEnvironment({}, output, scratch);
    expect(readJson(env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"])).toEqual(original);
    expect(readJson(source)).toEqual(original);
  });

  test("the Chrome command receives temporary capabilities, removed after it exits", async () => {
    const paths: string[] = [];
    await runChrome(
      ["wasm-pack"],
      { CHROME: "/selected/chromium", TMPDIR: scratch },
      scratch,
      (_command, options) => {
        const path = options.env["WASM_BINDGEN_TEST_WEBDRIVER_JSON"] ?? "";
        paths.push(path);
        expect(readJson(path)["goog:chromeOptions"]).toEqual({ binary: "/selected/chromium" });
        expect(options.cwd).toBe(join(ROOT, "clients/phux-web"));
        expect(options.env["CARGO_TARGET_DIR"]).toBe(join(options.cwd, "target"));
        return Promise.resolve();
      },
    );
    expect(paths).toHaveLength(1);
    expect(dirname(paths[0] ?? "")).toStartWith(join(scratch, "phux-webdriver-"));
    expect(existsSync(paths[0] ?? "")).toBe(false);
  });
});

describe("owned process groups", () => {
  function withLog<T>(body: (log: number) => Promise<T>): Promise<T> {
    const log = openSync(join(scratch, "run.log"), "w");
    return body(log).finally(() => closeSync(log));
  }

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

  // The descendant ignores SIGINT on purpose, so stop() always spends its
  // whole grace before escalating; the escalation is what is under test, not
  // the production grace's length.
  async function checkDescendantCleanup(parentExits: boolean): Promise<void> {
    // Single-quoted, so `$$` is the descendant's own pid.
    const child = `'trap "" INT; echo $$ > child; exec sleep 60'`;
    const parent =
      `echo $$ > group; sh -c ${child} & ` +
      `while [ ! -s child ]; do sleep 0.01; done; sleep ${parentExits ? 0 : 60}`;
    const options = {
      cwd: scratch,
      env: { PATH: process.env["PATH"] ?? "/usr/bin:/bin" },
      graceMs: 500,
    };
    try {
      await withLog(async (log) => {
        if (parentExits) {
          await run(["sh", "-c", parent], { ...options, timeoutMs: 5000, log });
        } else {
          const error = await failure(
            run(["sh", "-c", parent], { ...options, timeoutMs: 1000, log }),
          );
          expect(error).toBeInstanceOf(CommandTimeout);
        }
      });
      await assertStopped(Number(readFileSync(join(scratch, "child"), "utf8")));
    } finally {
      // Clean up even when run() regresses: only this fixture's own group.
      const group = join(scratch, "group");
      if (existsSync(group)) signalGroup(Number(readFileSync(group, "utf8")), "SIGKILL");
    }
  }

  test("a timeout stops a SIGINT-ignoring descendant", () => checkDescendantCleanup(false));

  test("an exited leader still stops a SIGINT-ignoring descendant", () =>
    checkDescendantCleanup(true));

  test("a command failure is not masked", async () => {
    const error = await withLog((log) =>
      failure(run(["sh", "-c", "exit 7"], { cwd: ROOT, env: {}, timeoutMs: 5000, log })),
    );
    expect(error).toBeInstanceOf(CommandFailed);
    expect(error instanceof CommandFailed && error.code).toBe(7);
  });

  test("a timeout reaps its child", async () => {
    const script = "echo $$ > pid; exec sleep 30";
    const error = await withLog((log) =>
      failure(run(["sh", "-c", script], { cwd: scratch, env: {}, timeoutMs: 1000, log })),
    );
    expect(error).toBeInstanceOf(CommandTimeout);
    const pid = Number(readFileSync(join(scratch, "pid"), "utf8"));
    expect(() => process.kill(pid, 0)).toThrow();
  });

  test("an unsignalable zombie group is already stopped", async () => {
    // A successful run must not turn into a failure when Darwin refuses to
    // signal a group that holds only zombies.
    const signals: (NodeJS.Signals | 0)[] = [];
    let waited = 0;
    const child = {
      pid: 12345,
      get exited() {
        waited += 1;
        return Promise.resolve(0);
      },
    };
    await stop(child, 10_000, (_group, signal) => {
      signals.push(signal);
      return false;
    });
    expect(signals).toEqual(["SIGINT", 0]);
    expect(waited).toBe(1);
  });

  test("a group that no longer exists is not signalable", async () => {
    const child = Bun.spawn(["true"], { detached: true });
    await child.exited;
    expect(signalGroup(child.pid, 0)).toBe(false);
  });
});

describe("browser test evidence", () => {
  test("node success is not browser execution", () => {
    expect(() => requireBrowserTests("no tests to run!\ntest result: ok. 0 passed")).toThrow(
      "renders_engine_grid_to_canvas",
    );
  });

  test("every required browser test must pass", () => {
    const output = REQUIRED_TESTS.map((name) => `test ${name} ... ok`).join("\n");
    expect(requireBrowserTests(output)).toContain(`${REQUIRED_TESTS.length} required`);
    expect(() => requireBrowserTests(output.replace("synthesized_only", "skipped_only"))).toThrow(
      "synthesized_only",
    );
  });

  test("module-qualified test names count", () => {
    expect(requireBrowserTests("test e2e::blackholed ... ok", ["blackholed"])).toContain(
      "1 required",
    );
  });
});

describe("demo server readiness", () => {
  const running = { exitCode: null };

  test("an early server exit is not readiness", async () => {
    expect((await failure(waitReady({ exitCode: 1 }, 1))).message).toContain("exited before");
  });

  test("readiness is bounded", async () => {
    let probes = 0;
    const probe = () => {
      probes += 1;
      return Promise.reject(new Error("connection refused"));
    };
    const error = await failure(waitReady(running, 1, { probe, pause: noPause }));
    expect(error).toBeInstanceOf(CommandTimeout);
    expect(probes).toBe(120);
  });

  test("readiness waits for the WebSocket upgrade", async () => {
    const answers = [false, true];
    let probes = 0;
    const probe = () => {
      probes += 1;
      return Promise.resolve(answers.shift() ?? false);
    };
    await waitReady(running, 1, { probe, pause: noPause });
    expect(probes).toBe(2);
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

  test("the bound port is read from the listening line", async () => {
    const log = join(scratch, "server.log");
    writeFileSync(log, "ws-demo-server listening on wss://127.0.0.1:43127/  (seed: default)\n");
    expect(await boundPort(running, log)).toBe(43127);
  });

  test("the bound port waits for the line and is bounded", async () => {
    const log = join(scratch, "server.log");
    writeFileSync(log, "");
    let pauses = 0;
    const pause = () => {
      pauses += 1;
      return Promise.resolve();
    };
    expect(await failure(boundPort(running, log, pause))).toBeInstanceOf(CommandTimeout);
    expect(pauses).toBe(300);
  });

  test("an early server exit is not a bound port", async () => {
    const log = join(scratch, "server.log");
    writeFileSync(log, "");
    expect((await failure(boundPort({ exitCode: 1 }, log))).message).toContain("exited before");
  });
});

test("inherited phux configuration never reaches the demo server", () => {
  const env = isolatedEnvironment({
    PATH: "/bin",
    PHUX_SOCKET: "/tmp/phux-user/phux.sock",
    PHUX_QUIC_ADDR: "0.0.0.0:8788",
    PHUX_WS_TOKENS: "/home/user/.local/state/phux/remote-tokens",
    PHUX_WS_TLS_CERT: "/home/user/.local/state/phux/remote-cert.pem",
    PHUX_WEB_CARGO_TARGET_DIR: "/scratch/web",
    PHUX_BROWSER_AUTH_ONLY: "1",
  });
  expect(env).toEqual({
    PATH: "/bin",
    PHUX_WEB_CARGO_TARGET_DIR: "/scratch/web",
    PHUX_BROWSER_AUTH_ONLY: "1",
  });
});

describe("browser scratch", () => {
  const bytes = (path: string) => Buffer.byteLength(path);

  function assertScratchSocketsFit(scratchDir: string, limit: number): void {
    // Literals, not the runner's constants: a shorter budget must not
    // silently satisfy this check. The created directory is the TMPDIR child
    // processes inherit.
    const sockets = [
      `${scratchDir}/mbx-session-XXXXXXXX/cache-agent.sock`,
      `${scratchDir}/phux-e2e-${"0".repeat(10)}.sock`,
      `${scratchDir}/phux-webdriver-${"x".repeat(8)}`,
    ];
    for (const path of sockets) expect(bytes(path)).toBeLessThanOrEqual(limit);
    expect(NESTED_SOCKET_SUFFIXES).toContain("mbx-session-XXXXXXXX/cache-agent.sock");
    expect(NESTED_SOCKET_SUFFIXES).toContain(`phux-e2e-${"0".repeat(10)}.sock`);
    expect(NESTED_SOCKET_SUFFIXES).toContain(`phux-webdriver-${"x".repeat(8)}`);
    const name = basename(scratchDir);
    expect(name).toStartWith("phux-browser-");
    expect(name.length - "phux-browser-".length).toBeLessThanOrEqual(TEMP_NAME_RANDOM);
  }

  function withScratch(base: string, body: (dir: string) => void): string {
    const dir = browserScratchDir(base);
    try {
      body(dir);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
    return dir;
  }

  test("a long macOS TMPDIR moves the scratch under the Darwin limit", () => {
    // A real macOS user temp directory: /var/folders/<2>/<30>/T.
    const longTmp = "/var/folders/3m/8h8q2n1d4c5b6a7z8y9x0w1v2u3t4s/T";
    const naive = `${longTmp}/phux-browser-${"x".repeat(8)}/mbx-session-XXXXXXXX/cache-agent.sock`;
    expect(bytes(naive)).toBeGreaterThan(DARWIN_SOCKET_PATH_MAX);
    const base = browserScratchBase({ TMPDIR: longTmp }, { platform: "darwin" });
    expect(base).toBe("/tmp");
    const made = withScratch(base, (dir) => {
      expect(existsSync(dir)).toBe(true);
      expect(dirname(dir)).toBe("/tmp");
      expect(dir.startsWith(longTmp)).toBe(false);
      assertScratchSocketsFit(dir, DARWIN_SOCKET_PATH_MAX);
    });
    expect(existsSync(made)).toBe(false);
  });

  test("Linux keeps a TMPDIR that still fits its own limit", () => {
    // One byte past Darwin's limit still fits Linux. Hosted Linux CI must
    // keep a TMPDIR in that window instead of moving it.
    let base = "/var/tmpdirs";
    const length = (path: string) =>
      bytes(`${path}/phux-browser-${"x".repeat(8)}/mbx-session-XXXXXXXX/cache-agent.sock`);
    while (length(base) <= DARWIN_SOCKET_PATH_MAX) base += "p";
    expect(length(base)).toBeLessThanOrEqual(LINUX_SOCKET_PATH_MAX);
    expect(length(base)).toBeGreaterThan(DARWIN_SOCKET_PATH_MAX);
    expect(browserScratchBase({ TMPDIR: base }, { platform: "linux" })).toBe(base);
    expect(browserScratchBase({ TMPDIR: base }, { platform: "darwin" })).toBe("/tmp");
  });

  test("a short caller TMPDIR is the scratch parent", () => {
    const caller = mkdtempSync("/tmp/phux-caller-");
    try {
      const base = browserScratchBase(
        { TMPDIR: caller },
        { platform: "darwin", candidates: ["/no/such/phux-short-base"] },
      );
      expect(base).toBe(caller);
      withScratch(base, (dir) => {
        expect(dirname(dir)).toBe(caller);
        assertScratchSocketsFit(dir, DARWIN_SOCKET_PATH_MAX);
      });
    } finally {
      rmSync(caller, { recursive: true, force: true });
    }
  });

  test("an unset TMPDIR keeps a short default", () => {
    const fallback = tmpdir();
    for (const platform of ["linux", "darwin"]) {
      const chosen = browserScratchBase({}, { platform });
      const naive = `${fallback}/phux-browser-${"x".repeat(8)}/mbx-session-XXXXXXXX/cache-agent.sock`;
      if (bytes(naive) <= socketPathMax(platform)) expect(chosen).toBe(fallback);
      else expect(chosen).not.toBe(fallback);
      withScratch(chosen, (dir) => assertScratchSocketsFit(dir, socketPathMax(platform)));
    }
  });

  test("no short scratch base names the socket limit", () => {
    const longTmp = `/var/folders/xx/${"y".repeat(30)}/T`;
    let message = "";
    try {
      browserScratchBase(
        { TMPDIR: longTmp },
        { platform: "darwin", candidates: ["/no/such/phux-short-base"] },
      );
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }
    expect(message).toContain("TMPDIR");
    expect(message).toContain("/tmp");
    expect(message).toContain(String(DARWIN_SOCKET_PATH_MAX));
    expect(message).toContain("mbx-session-XXXXXXXX/cache-agent.sock");
    expect(message).toContain(longTmp);
  });
});
