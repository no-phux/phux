/** Packaged macOS smoke: no installed app, production socket, or focus activation. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { assertPresentedEvidence } from "./evidence.ts";
import { isInstalledPhux, layoutFile, productionSocketReason } from "./host.ts";

const { values } = parseArgs({
  options: {
    app: { type: "string" },
    phux: { type: "string" },
    output: { type: "string" },
    // Hosted CI has only a virtual display, which never confirms a drawable.
    "allow-unconfirmed-present": { type: "boolean" },
    help: { type: "boolean" },
  },
});
if (values.help) {
  console.log(
    "bun clients/desktop/scripts/smoke-app.ts --app <Phux.app> --phux <same-checkout-cli> [--output <evidence-dir>] [--allow-unconfirmed-present]",
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
const allowSubmitted = values["allow-unconfirmed-present"] === true;
const appPath = resolve(values.app);
const executable = join(appPath, "Contents/MacOS/phux-desktop");
const phux = resolve(values.phux);
assert(existsSync(executable) && existsSync(phux), "App executable and explicit CLI must exist");
assert(
  !existsSync(join(appPath, "Contents/Resources/phux-desktop-native.darwin-arm64.node")),
  "Phux.app must not ship a NAPI addon",
);
assert(!isInstalledPhux(phux), "smoke refuses an installed phux CLI");
const home = mkdtempSync("/tmp/phux-desktop-smoke-");
const evidence = values.output ? resolve(values.output) : join(home, "evidence");
mkdirSync(evidence, { recursive: true });
for (const dir of ["config", "state", "cache", "data", "run"]) mkdirSync(join(home, dir));
const socket = join(home, "run/phux.sock");
const session = "desktop-smoke";
const socketReason = productionSocketReason(socket, {
  ...process.env,
  TMPDIR: home,
  XDG_RUNTIME_DIR: join(home, "run"),
});
assert(socketReason === undefined, socketReason ?? "isolated socket");
const layout = layoutFile(join(home, "state"), socket, session);
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
  PHUX_SESSION: session,
  PHUX_BIN: phux,
  PHUX_DESKTOP_BACKGROUND: "1",
  // The app's stderr is retained evidence: keep the presentation, visibility
  // and acquire decisions in it so a failed qualification explains itself.
  PHUX_DESKTOP_LOG_LEVEL: "trace",
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
  if (result.error || result.status !== 0) {
    throw new Error(`${command.join(" ")}: ${result.error?.message ?? result.stderr}`);
  }
  return result.stdout;
}
function cli(...args: string[]): string {
  return run([phux, "--socket", socket, ...args]);
}
async function until(label: string, check: () => boolean, child?: Bun.Subprocess): Promise<void> {
  const deadline = Date.now() + 30_000;
  let last = "condition not observed";
  do {
    if (child && child.exitCode !== null) {
      throw new Error(`${label}: process exited ${child.exitCode}; logs in ${evidence}`);
    }
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
const ocr = join(home, "read-image.swift");
writeFileSync(
  ocr,
  `import Foundation
import Vision
import CoreImage
let url = URL(fileURLWithPath: CommandLine.arguments[1])
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
const projectName = "Cedar";
const projectSession = "Spruce";
const marker = "PHUXSURVIVAL";
let server: Bun.Subprocess | undefined;
function appArgs(dir: string, smoke: boolean): string[] {
  const args = [
    executable,
    "--socket",
    socket,
    "--session",
    session,
    "--phux",
    phux,
    "--layout",
    layout,
    "--evidence",
    dir,
  ];
  if (smoke) args.push("--smoke");
  return args;
}
/** How each capture was shown: confirmed by the display, or only submitted. */
const presentations: Record<string, string> = {};
async function capture(label: string, expected = marker): Promise<Bun.Subprocess> {
  const dir = join(evidence, label);
  mkdirSync(dir, { recursive: true });
  const app = launch(appArgs(dir, false), label);
  await until(
    `${label} presented`,
    () => {
      presentations[label] = assertPresentedEvidence(dir, session, allowSubmitted).presentation;
      return true;
    },
    app,
  );
  const text = run(["/usr/bin/swift", ocr, join(dir, "presented.png")], 120_000);
  writeFileSync(join(evidence, `${label}.ocr.txt`), text);
  assert(text.includes(expected), `${label}: terminal marker not rendered; OCR: ${text}`);
  assert(
    text.toLowerCase().includes(projectName.toLowerCase()),
    `${label}: project sidebar not rendered; OCR: ${text}`,
  );
  assert(
    text.toLowerCase().includes("recent folders"),
    `${label}: persistent recent folder shortcuts not rendered; OCR: ${text}`,
  );
  return app;
}
async function cleanup(): Promise<void> {
  for (const child of children.toReversed()) await stop(child, "SIGTERM");
}
let interrupted = false;
for (const signal of ["SIGINT", "SIGTERM"] as const) {
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
}
try {
  server = launch(
    [phux, "--socket", socket, "server", "--session", session, "--exit-after-idle", "120"],
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
  cli(
    "send-keys",
    session,
    `PHUX_SMOKE_VALUE=${marker}; printf '%s\\n' "$$" > '${pidFile}'; printf '\\033[2J\\033[H%s\\n' "$PHUX_SMOKE_VALUE"`,
    "Enter",
  );
  await until(
    "terminal marker",
    () => cli("snapshot", session).includes(marker) && existsSync(pidFile),
    server,
  );
  const shellPid = readFileSync(pidFile, "utf8").trim();
  assert(/^\d+$/.test(shellPid), "Shell must report its own PID");
  const smokeDir = join(evidence, "smoke-exit");
  mkdirSync(smokeDir, { recursive: true });
  const smoked = launch(appArgs(smokeDir, true), "smoke-exit");
  await until(
    "smoke exit",
    () => {
      if (smoked.exitCode === null) return false;
      if (smoked.exitCode !== 0) throw new Error(`--smoke exited ${smoked.exitCode}`);
      presentations["smoke-exit"] = assertPresentedEvidence(
        smokeDir,
        session,
        allowSubmitted,
      ).presentation;
      return true;
    },
    undefined,
  );
  // The session name differs from its Git project label: OCR of "Cedar"
  // therefore proves grouping, not just that the session row rendered.
  const repository = join(home, projectName);
  const projectDir = join(home, projectSession);
  run(["/usr/bin/git", "init", "-q", repository]);
  run([
    "/usr/bin/git",
    "-C",
    repository,
    "-c",
    "user.name=Desktop Smoke",
    "-c",
    "user.email=desktop-smoke@localhost",
    "-c",
    "core.hooksPath=/dev/null",
    "commit",
    "--allow-empty",
    "-qm",
    "seed",
  ]);
  run(["/usr/bin/git", "-C", repository, "worktree", "add", "-q", "-b", "spruce", projectDir]);
  // The app's local history is scoped to this socket, independent of the
  // server Session; rendering it survives each fresh app process below.
  const recentFile = join(dirname(layoutFile(join(home, "state"), socket, "")), "projects.json");
  mkdirSync(dirname(recentFile), { recursive: true });
  writeFileSync(recentFile, JSON.stringify([projectDir]));
  const opened: unknown = JSON.parse(cli("project", "open", "--json", projectDir));
  assert(
    opened &&
      typeof opened === "object" &&
      "session" in opened &&
      opened.session === projectSession,
    "Project CLI must create the checkout-bound worktree session before desktop attach",
  );
  const first = await capture("first-launch");
  await stop(first, "SIGKILL");
  assert.equal(server.exitCode, null, "App death must not kill the server");
  const crashPidFile = join(home, "shell-after-crash.pid");
  cli(
    "send-keys",
    session,
    `printf '%s\\n' "$$" > '${crashPidFile}'; printf '\\033[2J\\033[H%s%s\\n' "$PHUX_SMOKE_VALUE" AFTERCRASH`,
    "Enter",
  );
  await until(
    "same live shell after app crash",
    () =>
      cli("snapshot", session).includes(`${marker}AFTERCRASH`) &&
      existsSync(crashPidFile) &&
      readFileSync(crashPidFile, "utf8").trim() === shellPid,
    server,
  );
  const second = await capture("after-crash", `${marker}AFTERCRASH`);
  await stop(second, "SIGTERM");
  assert.equal(server.exitCode, null, "Stopping the app must not kill the server");
  const stopPidFile = join(home, "shell-after-stop.pid");
  cli(
    "send-keys",
    session,
    `printf '%s\\n' "$$" > '${stopPidFile}'; printf '\\033[2J\\033[H%s%s\\n' "$PHUX_SMOKE_VALUE" AFTERSTOP`,
    "Enter",
  );
  await until(
    "same live shell after app stop",
    () =>
      cli("snapshot", session).includes(`${marker}AFTERSTOP`) &&
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
    scenarios: ["smoke-exit", "launch", "SIGKILL/relaunch", "SIGTERM/relaunch"],
    presentations,
    boundary:
      "App process death, not server death/reboot; PNG marker verified with Vision OCR and presented layout",
  };
  writeFileSync(join(evidence, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify(result, null, 2));
} finally {
  await cleanup();
  console.error(`Desktop smoke evidence retained at ${evidence}; isolated state at ${home}`);
}
