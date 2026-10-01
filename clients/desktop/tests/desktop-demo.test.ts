import { afterEach, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { checkoutArtifacts } from "../scripts/desktop-demo";
import { terminateChild } from "../scripts/terminate-child";

const desktop = resolve(import.meta.dir, "..");
const cleanup: string[] = [];
afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function fixture(): Promise<{ directory: string; binary: string; addon: string }> {
  // Real child processes and a real private UDS exercise ownership, not mocked spawn calls.
  const directory = await mkdtemp(resolve(desktop, ".demo-test-"));
  cleanup.push(directory);
  const binary = resolve(directory, "phux");
  const addon = resolve(directory, "addon.node");
  await writeFile(addon, "test artifact");
  await writeFile(
    binary,
    `#!${process.execPath}
import { createConnection, createServer } from "node:net";
import { writeFileSync } from "node:fs";
const socket = process.argv[process.argv.indexOf("--socket") + 1];
if (process.argv.includes("status")) {
  const connection = createConnection(socket);
  connection.once("connect", () => { connection.destroy(); process.exit(0); });
  connection.once("error", () => process.exit(1));
} else if (process.argv.includes("server")) {
  process.on("SIGTERM", () => {});
  writeFileSync(process.env.DEMO_TEST_SERVER, JSON.stringify({ pid: process.pid, env: process.env }));
  createServer((connection) => connection.end()).listen(socket);
} else process.exit(2);
`,
  );
  await chmod(binary, 0o700);
  return { directory, binary, addon };
}

interface Report {
  server: { pid: number; env: Record<string, string> };
  appPid?: number;
  home: string;
}

async function scenario(
  mode: string,
): Promise<{ code: number | null; report: Report; production: string }> {
  const { directory, binary, addon } = await fixture();
  const script = resolve(directory, "supervisor.ts");
  const record = resolve(directory, "report.json");
  const serverRecord = resolve(directory, "server.json");
  const production = resolve(directory, "production");
  await mkdir(production);
  await writeFile(resolve(production, "keep"), "untouched");
  await writeFile(
    script,
    `
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { runDemo } from ${JSON.stringify(resolve(desktop, "scripts/desktop-demo.ts"))};
const mode = ${JSON.stringify(mode)};
try {
  process.exitCode = await runDemo(${JSON.stringify({ binary, addon })}, (env) => {
    const report = { server: JSON.parse(readFileSync(${JSON.stringify(serverRecord)}, "utf8")), home: env.HOME };
    writeFileSync(${JSON.stringify(record)}, JSON.stringify(report));
    if (mode === "throw") throw new Error("launch refused");
    const program = mode === "spawn-error" ? "/nonexistent/phux-demo-app" : process.execPath;
    const code = mode === "normal"
      ? 'require("node:fs").writeFileSync(process.env.XDG_STATE_HOME + "/app-state", "private"); process.exit(7);'
      : 'process.on("SIGTERM", () => {}); console.log("ready"); setInterval(() => {}, 1000);';
    const app = spawn(program, ["-e", code], { env, stdio: ["ignore", "pipe", "inherit"] });
    report.appPid = app.pid;
    writeFileSync(${JSON.stringify(record)}, JSON.stringify(report));
    if (mode === "SIGTERM" || mode === "SIGINT")
      app.stdout.once("data", () => process.kill(process.pid, mode));
    if (mode === "server-exit")
      app.stdout.once("data", () => process.kill(report.server.pid, "SIGKILL"));
    return app;
  });
} catch (error) {
  console.error(error);
  process.exitCode = 1;
}
`,
  );
  const child = spawn(process.execPath, [script], {
    cwd: desktop,
    env: {
      ...process.env,
      HOME: production,
      XDG_CONFIG_HOME: production,
      XDG_CONFIG_DIRS: production,
      XDG_DATA_HOME: production,
      XDG_DATA_DIRS: production,
      XDG_STATE_HOME: production,
      XDG_CACHE_HOME: production,
      XDG_RUNTIME_DIR: production,
      XDG_UNEXPECTED_PATH: production,
      PHUX_SOCKET: resolve(production, "production.sock"),
      PHUX_PROFILE: "default",
      PHUX_BIN: "/nonexistent/production-phux",
      PHUX_WS_ADDR: "0.0.0.0:9999",
      PHUX_WS_TOKENS: "private-token",
      PHUX_WS_TLS_KEY: resolve(production, "secret.key"),
      PHUX_DESKTOP_CAPTURE: resolve(production, "capture.png"),
      DEMO_TEST_SERVER: serverRecord,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  // Child signal delivery/escalation uses the OS clock; fake timers cannot drive it.
  // Readiness is event-driven. This timer is only a failure bound, never a sleep.
  let timer: NodeJS.Timeout | undefined;
  try {
    const code = await Promise.race([
      new Promise<number | null>((done, reject) => {
        child.once("exit", done);
        child.once("error", reject);
      }),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Demo scenario ${mode} timed out`)), 12_000);
      }),
    ]);
    // SAFETY: this fixture's supervisor writes the fixed Report shape above.
    const report = JSON.parse(await readFile(record, "utf8")) as Report;
    expect(() => process.kill(report.server.pid, 0)).toThrow();
    const appPid = report.appPid;
    if (appPid !== undefined) expect(() => process.kill(appPid, 0)).toThrow();
    expect(existsSync(report.home)).toBe(false);
    expect(await readFile(resolve(production, "keep"), "utf8")).toBe("untouched");
    expect(existsSync(resolve(production, "app-state"))).toBe(false);
    return { code, report, production };
  } finally {
    clearTimeout(timer);
    await terminateChild(child);
  }
}

test("demo isolates server and app state from inherited production configuration", async () => {
  const { code, report, production } = await scenario("normal");
  expect(code).toBe(7);
  const env = report.server.env;
  expect(env.HOME).not.toBe(production);
  for (const key of [
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "TMPDIR",
  ]) {
    expect(env[key] === report.home || env[key]?.startsWith(`${report.home}/`)).toBe(true);
  }
  expect(env.PHUX_SOCKET).toBe(resolve(report.home, "demo.sock"));
  expect(env.PHUX_PROFILE).toBe("desktop-demo");
  expect(env.PHUX_NO_AUTO_LISTEN).toBe("1");
  expect(env.PHUX_WS_ADDR).toBeUndefined();
  expect(env.PHUX_WS_TOKENS).toBeUndefined();
  expect(env.PHUX_WS_TLS_KEY).toBeUndefined();
  expect(env.PHUX_DESKTOP_CAPTURE).toBeUndefined();
  expect(env.XDG_UNEXPECTED_PATH).toBeUndefined();
}, 15_000);

test("demo refuses external artifacts, including checkout symlinks escaping to them", async () => {
  const { directory, binary, addon } = await fixture();
  const outside = await mkdtemp(resolve(tmpdir(), "phux-demo-external-"));
  cleanup.push(outside);
  const external = resolve(outside, "installed-phux");
  await writeFile(external, "must never execute");
  const link = resolve(directory, "external-link");
  await symlink(external, link);
  expect(await checkoutArtifacts(external, addon).catch(String)).toContain("checkout-built");
  expect(await checkoutArtifacts(link, addon).catch(String)).toContain("checkout-built");
  expect(await checkoutArtifacts(binary, link).catch(String)).toContain("checkout-built");
});

for (const mode of ["throw", "spawn-error", "server-exit"]) {
  test(`demo reaps its owned children after ${mode}`, async () => {
    expect((await scenario(mode)).code).toBe(1);
  }, 15_000);
}

for (const [signal, code] of [
  ["SIGTERM", 143],
  ["SIGINT", 130],
] as const) {
  test(`demo handles ${signal} even when both children ignore SIGTERM`, async () => {
    expect((await scenario(signal)).code).toBe(code);
  }, 15_000);
}
