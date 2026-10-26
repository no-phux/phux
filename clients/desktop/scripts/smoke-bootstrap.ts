/** Genuine first launch: no installed CLI, server, explicit --phux or inherited PATH. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";

const { values } = parseArgs({ options: { app: { type: "string" }, output: { type: "string" } } });
assert(values.app, "--app is required; never discovers an installed app");
const app = resolve(values.app);
assert(
  !app.startsWith("/Applications/") && !app.startsWith(`${process.env.HOME}/Applications/`),
  "Refusing installed app",
);
const executable = join(app, "Contents/MacOS/phux-desktop");
const bundled = join(app, "Contents/MacOS/phux");
assert(existsSync(executable) && existsSync(bundled), "Bundle must include Desktop and CLI/server");
const home = mkdtempSync("/tmp/phux-bootstrap-home-");
const evidence = values.output
  ? resolve(values.output)
  : mkdtempSync("/tmp/phux-bootstrap-evidence-");
mkdirSync(evidence, { recursive: true });
const cli = join(home, ".local/bin/phux");
const env = {
  HOME: home,
  PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
  SHELL: "/bin/sh",
  TERM: "xterm-256color",
  LANG: "en_US.UTF-8",
  XDG_CONFIG_HOME: join(home, "config"),
  XDG_STATE_HOME: join(home, "state"),
  XDG_DATA_HOME: join(home, "data"),
  XDG_CACHE_HOME: join(home, "cache"),
  XDG_RUNTIME_DIR: join(home, "run"),
  TMPDIR: home,
  PHUX_PROFILE: "desktop-bootstrap",
  PHUX_DESKTOP_BACKGROUND: "1",
  PHUX_NO_UPDATE_CHECK: "1",
};
for (const name of ["config", "state", "data", "cache", "run"]) mkdirSync(join(home, name));
function run(args: string[], allowFailure = false): string {
  const result = spawnSync(cli, args, { env, cwd: home, encoding: "utf8", timeout: 30_000 });
  if (!allowFailure)
    assert(result.status === 0 && !result.error, result.stderr || String(result.error));
  return result.stdout;
}
async function launch(label: string): Promise<void> {
  const dir = join(evidence, label);
  mkdirSync(dir, { recursive: true });
  const child = Bun.spawn(
    [executable, "--session", "bootstrap-probe", "--evidence", dir, "--smoke"],
    {
      env,
      cwd: home,
      stdin: "ignore",
      stdout: Bun.file(join(dir, "stdout.log")),
      stderr: Bun.file(join(dir, "stderr.log")),
    },
  );
  const code = await Promise.race([child.exited, Bun.sleep(60_000).then(() => undefined)]);
  if (code === undefined) {
    child.kill("SIGKILL");
    await child.exited;
  }
  assert.equal(code, 0, `${label}: app did not finish the presented-frame probe; see ${dir}`);
  assert(existsSync(join(dir, "presented.png")), `${label}: no first-launch screen captured`);
}
function status(): { running: boolean; pid: number; socket: string } {
  const value: unknown = JSON.parse(run(["status", "--json"]));
  assert(value !== null && typeof value === "object");
  assert("running" in value && "pid" in value && "socket" in value);
  const { running, pid, socket } = value;
  assert(typeof running === "boolean" && typeof pid === "number" && typeof socket === "string");
  return { running, pid, socket };
}
async function launchFailure(label: string): Promise<void> {
  const dir = join(evidence, label);
  mkdirSync(dir, { recursive: true });
  const log = join(dir, "stderr.log");
  const child = Bun.spawn([executable, "--evidence", dir], {
    env,
    cwd: home,
    stdin: "ignore",
    stdout: Bun.file(join(dir, "stdout.log")),
    stderr: Bun.file(log),
  });
  try {
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      assert(
        child.exitCode === null,
        "Startup failure crashed the app instead of leaving a repairable window",
      );
      if (existsSync(log) && readFileSync(log, "utf8").includes("error=Some")) return;
      await Bun.sleep(100);
    }
    throw new Error(`No actionable startup failure was reported; see ${dir}`);
  } finally {
    if (child.exitCode === null) child.kill("SIGKILL");
    await child.exited;
  }
}

const hash = (path: string) => createHash("sha256").update(readFileSync(path)).digest("hex");
try {
  assert(!existsSync(cli));
  await launch("clean-dmg");
  assert(existsSync(cli), "Fresh app failed to install its bundled runtime");
  assert.equal(hash(cli), hash(bundled), "First launch did not install the exact bundled runtime");
  const first = status();
  assert(first.running && first.socket.startsWith(home), "App did not start an isolated server");
  await launch("existing-cli-and-server");
  assert.equal(status().pid, first.pid, "Reopening Desktop restarted a healthy server");
  run(["kill", "--server", "--yes"]);
  await launch("stopped-server");
  assert(status().running, "Desktop did not restart its stopped local server");
  run(["kill", "--server", "--yes"]);
  mkdirSync(join(home, "config/phux"), { recursive: true });
  writeFileSync(join(home, "config/phux/config.toml"), "[invalid config\n");
  await launchFailure("invalid-config");
  const log = readFileSync(join(evidence, "invalid-config/stderr.log"), "utf8");
  assert(log.includes("error=Some"), "Invalid config was not reported as a startup error");
  rmSync(join(home, "config/phux/config.toml"));
  await launch("repaired-config");
  assert(status().running, "Desktop did not recover after config was repaired");
  writeFileSync(
    join(evidence, "bootstrap.json"),
    JSON.stringify(
      {
        bundledRuntime: true,
        installedExactBytes: true,
        reusedServer: true,
        restartedStoppedServer: true,
        invalidConfigReported: true,
        repairedConfig: true,
        firstSocket: first.socket,
      },
      null,
      2,
    ),
  );
  console.log(`Packaged bootstrap journeys passed: ${evidence}`);
} finally {
  if (existsSync(cli)) run(["kill", "--server", "--yes"], true);
  rmSync(home, { recursive: true, force: true });
}
