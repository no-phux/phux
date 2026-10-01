/** Packaged macOS smoke: no installed app, production socket, or focus activation. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";

const { values } = parseArgs({
  options: {
    app: { type: "string" },
    phux: { type: "string" },
    output: { type: "string" },
    help: { type: "boolean" },
  },
});
if (values.help) {
  console.log(
    "bun clients/desktop/scripts/smoke-app.ts --app <Phux.app> --phux <same-checkout-cli> [--output <evidence-dir>]",
  );
  process.exit(0);
}
assert(
  values.app && values.phux,
  "--app and --phux are required (never discovers an installed CLI)",
);
assert(
  process.platform === "darwin" && process.arch === "arm64",
  "Requires Apple silicon macOS with a graphical login and Xcode command-line tools",
);
const executable = join(resolve(values.app), "Contents/MacOS/phux-desktop");
const phux = resolve(values.phux);
assert(existsSync(executable) && existsSync(phux), "App executable and explicit CLI must exist");
// Short path stays below the Unix-domain socket path limit even on macOS.
const home = mkdtempSync("/tmp/phux-desktop-smoke-");
const evidence = values.output ? resolve(values.output) : join(home, "evidence");
mkdirSync(evidence, { recursive: true });
for (const dir of ["config", "state", "cache", "data", "run"]) mkdirSync(join(home, dir));
const socket = join(home, "run/phux.sock");
// Whitelist rather than deleting known hazards: no inherited PHUX_WS_*, TLS,
// tokens, addon/preload overrides, config, or workspace identity can leak in.
const env: Record<string, string> = {
  PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
  HOME: home,
  XDG_CONFIG_HOME: join(home, "config"),
  XDG_STATE_HOME: join(home, "state"),
  XDG_CACHE_HOME: join(home, "cache"),
  XDG_DATA_HOME: join(home, "data"),
  XDG_RUNTIME_DIR: join(home, "run"),
  TMPDIR: home,
  PHUX_PROFILE: "desktop-smoke",
  PHUX_SOCKET: socket,
  PHUX_SESSION: "desktop-smoke",
  PHUX_BIN: phux,
  PHUX_DESKTOP_BACKGROUND: "1",
  SHELL: "/bin/sh",
  TERM: "xterm-256color",
  LANG: "en_US.UTF-8",
  PS1: "smoke> ",
};
const children: Bun.Subprocess[] = [];
function launch(
  command: string[],
  log: string,
  extra: Record<string, string> = {},
): Bun.Subprocess {
  const child = Bun.spawn(command, {
    cwd: home,
    env: { ...env, ...extra },
    stdin: "ignore",
    stdout: Bun.file(join(evidence, `${log}.stdout.log`)),
    stderr: Bun.file(join(evidence, `${log}.stderr.log`)),
  });
  children.push(child);
  return child;
}
function run(command: string[], timeout = 30_000): string {
  const result = spawnSync(command[0]!, command.slice(1), {
    cwd: home,
    env,
    encoding: "utf8",
    timeout,
  });
  if (result.error || result.status !== 0)
    throw new Error(`${command.join(" ")}: ${result.error?.message ?? result.stderr}`);
  return result.stdout;
}
function cli(...args: string[]): string {
  return run([phux, "--socket", socket, ...args]);
}
async function until(label: string, check: () => boolean, child?: Bun.Subprocess): Promise<void> {
  const deadline = Date.now() + 30_000;
  let last = "condition not observed";
  do {
    if (child && child.exitCode !== null)
      throw new Error(`${label}: process exited ${child.exitCode}; logs in ${evidence}`);
    try {
      if (check()) return;
    } catch (error) {
      last = error instanceof Error ? error.message : "check failed";
    }
    await Bun.sleep(100);
  } while (Date.now() < deadline);
  throw new Error(`${label} timed out: ${last}`);
}
async function stop(child: Bun.Subprocess, signal: "SIGTERM" | "SIGKILL"): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill(signal);
  const exited = await Promise.race([
    child.exited.then(() => true),
    Bun.sleep(5000).then(() => false),
  ]);
  if (!exited) {
    child.kill("SIGKILL");
    await child.exited;
  }
}
// Vision reads the actual GPUI PNG, not the server's screen or UI source tree.
const ocr = join(home, "read-image.swift");
writeFileSync(
  ocr,
  `import Foundation
import Vision
import CoreImage
let url = URL(fileURLWithPath: CommandLine.arguments[1])
// Normal terminal fonts fall below Vision's reliable OCR size at 1x.
let scaled = CIImage(contentsOf: url)!.transformed(by: CGAffineTransform(scaleX: 2, y: 2))
let image = CIContext().createCGImage(scaled, from: scaled.extent)!
let request = VNRecognizeTextRequest()
request.recognitionLevel = .fast
request.minimumTextHeight = 0
request.recognitionLanguages = ["en-US"]
request.usesLanguageCorrection = false
try VNImageRequestHandler(cgImage: image).perform([request])
for result in request.results ?? [] {
    if let text = result.topCandidates(1).first?.string { print(text) }
}
`,
);
const marker = "PHUXSURVIVAL";
let server: Bun.Subprocess | undefined;
async function capture(label: string, expected = marker): Promise<Bun.Subprocess> {
  const image = join(evidence, `${label}.png`);
  rmSync(image, { force: true });
  const app = launch([executable], label, { PHUX_DESKTOP_CAPTURE: `3000:${image}` });
  await until(
    `${label} captured surface`,
    () => {
      if (!existsSync(image)) return false;
      const bytes = readFileSync(image);
      return (
        bytes.length > 24 &&
        bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
      );
    },
    app,
  );
  const text = run(["/usr/bin/swift", ocr, image], 120_000);
  writeFileSync(join(evidence, `${label}.ocr.txt`), text);
  assert(
    text.includes(expected),
    `${label}: terminal marker not rendered in ${image}; OCR: ${text}`,
  );
  return app;
}
async function cleanup(): Promise<void> {
  // Only child handles created above; never pkill/killall, LaunchServices, or a
  // discovered server pid. Graceful server shutdown owns cleanup of its PTYs.
  for (const child of children.toReversed()) await stop(child, "SIGTERM");
}
let interrupted = false;
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.once(signal, () => {
    if (interrupted) return;
    interrupted = true;
    cleanup().then(
      () => process.exit(130),
      (error: unknown) => {
        console.error(error);
        process.exit(130);
      },
    );
  });
try {
  server = launch(
    [phux, "--socket", socket, "server", "--session", "desktop-smoke", "--exit-after-idle", "120"],
    "server",
  );
  await until(
    "isolated server",
    () => {
      const status: unknown = JSON.parse(cli("status", "--json"));
      return (
        typeof status === "object" &&
        status !== null &&
        "running" in status &&
        status.running === true
      );
    },
    server,
  );
  const before: unknown = JSON.parse(cli("status", "--json"));
  writeFileSync(join(evidence, "status-before.json"), JSON.stringify(before, null, 2));
  const pidFile = join(home, "shell.pid");
  // Keep a non-exported shell variable: a replaced shell cannot reconstruct it.
  cli(
    "send-keys",
    "desktop-smoke",
    `PHUX_SMOKE_VALUE=${marker}; printf '%s\\n' "$$" > '${pidFile}'; printf '\\033[2J\\033[H%s\\n' "$PHUX_SMOKE_VALUE"`,
    "Enter",
  );
  await until(
    "terminal marker",
    () => cli("snapshot", "desktop-smoke").includes(marker) && existsSync(pidFile),
    server,
  );
  const shellPid = readFileSync(pidFile, "utf8").trim();
  assert(/^\d+$/.test(shellPid), "Shell must report its own PID");
  const first = await capture("first-launch");
  await stop(first, "SIGKILL");
  assert.equal(server.exitCode, null, "App death must not kill the server");
  const crashPidFile = join(home, "shell-after-crash.pid");
  cli(
    "send-keys",
    "desktop-smoke",
    `printf '%s\\n' "$$" > '${crashPidFile}'; printf '\\033[2J\\033[H%s%s\\n' "$PHUX_SMOKE_VALUE" AFTERCRASH`,
    "Enter",
  );
  await until(
    "same live shell after app crash",
    () => {
      const screen = cli("snapshot", "desktop-smoke");
      return (
        screen.includes(`${marker}AFTERCRASH`) &&
        existsSync(crashPidFile) &&
        readFileSync(crashPidFile, "utf8").trim() === shellPid
      );
    },
    server,
  );
  const second = await capture("after-crash", `${marker}AFTERCRASH`);
  await stop(second, "SIGTERM");
  assert.equal(server.exitCode, null, "Stopping the app must not kill the server");
  const stopPidFile = join(home, "shell-after-stop.pid");
  cli(
    "send-keys",
    "desktop-smoke",
    `printf '%s\\n' "$$" > '${stopPidFile}'; printf '\\033[2J\\033[H%s%s\\n' "$PHUX_SMOKE_VALUE" AFTERSTOP`,
    "Enter",
  );
  await until(
    "same live shell after app stop",
    () =>
      cli("snapshot", "desktop-smoke").includes(`${marker}AFTERSTOP`) &&
      existsSync(stopPidFile) &&
      readFileSync(stopPidFile, "utf8").trim() === shellPid,
    server,
  );
  const third = await capture("after-stop", `${marker}AFTERSTOP`);
  assert.equal(third.exitCode, null);
  const after: unknown = JSON.parse(cli("status", "--json"));
  assert(
    before &&
      after &&
      typeof before === "object" &&
      typeof after === "object" &&
      "pid" in before &&
      "pid" in after,
  );
  assert.equal(after.pid, before.pid, "The original server process must survive every app launch");
  assert.equal(after.pid, server.pid, "Smoke must attach only its directly owned server");
  writeFileSync(join(evidence, "status-after.json"), JSON.stringify(after, null, 2));
  const result = {
    ok: true,
    serverPid: server.pid,
    shellPid,
    marker,
    evidence,
    scenarios: ["launch", "SIGKILL/relaunch", "SIGTERM/relaunch"],
    boundary: "App process death, not server death/reboot; PNG marker verified with Vision OCR",
  };
  writeFileSync(join(evidence, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify(result, null, 2));
} finally {
  await cleanup();
  console.error(`Desktop smoke evidence retained at ${evidence}; isolated state at ${home}`);
}
