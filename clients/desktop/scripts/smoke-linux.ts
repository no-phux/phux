/** Archived Linux desktop qualification under Xvfb/lavapipe; never production state. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { assertPresentedEvidence } from "./evidence.ts";
import { isInstalledPhux, layoutFile, productionSocketReason } from "./host.ts";

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
    "xvfb-run -a dbus-run-session -- bun clients/desktop/scripts/smoke-linux.ts --app <phux-desktop-bundle> --phux <same-checkout-cli> [--output <evidence-dir>]",
  );
  process.exit(0);
}
assert(values.app && values.phux, "--app and explicit --phux are required");
assert(process.platform === "linux", "Linux qualification requires a Linux runner");
assert(process.env.DISPLAY, "Run under Xvfb: the qualification backend is X11");
const executable = join(resolve(values.app), "bin/phux-desktop");
const phux = resolve(values.phux);
assert(existsSync(executable) && existsSync(phux), "Bundle and explicit CLI must exist");
assert(!isInstalledPhux(phux), "qualification refuses an installed phux CLI");
const home = mkdtempSync("/tmp/phux-linux-smoke-");
const evidence = values.output ? resolve(values.output) : join(home, "evidence");
mkdirSync(evidence, { recursive: true });
for (const dir of ["config", "state", "cache", "data", "run"]) {
  mkdirSync(join(home, dir), { mode: 0o700 });
}
const socket = join(home, "run/phux.sock");
const session = "desktop-smoke";
const layout = layoutFile(join(home, "state"), socket, session);
assert.equal(productionSocketReason(socket, { TMPDIR: home }), undefined);
// Allow only display/software-driver settings, never inherited PHUX_* auth,
// production sockets, user configuration, Wayland selection or preload paths.
const env: Record<string, string> = {
  PATH: "/usr/bin:/bin",
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
  PHUX_DESKTOP_LOG_LEVEL: "trace",
  SHELL: "/bin/sh",
  TERM: "xterm-256color",
  LANG: "C.UTF-8",
  PS1: "smoke> ",
};
for (const key of [
  "DISPLAY",
  "XAUTHORITY",
  "DBUS_SESSION_BUS_ADDRESS",
  "VK_DRIVER_FILES",
  "VK_ICD_FILENAMES",
  "WGPU_BACKEND",
  "LIBGL_ALWAYS_SOFTWARE",
]) {
  const value = process.env[key];
  if (value) env[key] = value;
}
const children: Bun.Subprocess[] = [];
function launch(command: string[], label: string): Bun.Subprocess {
  const child = Bun.spawn(command, {
    cwd: home,
    env,
    stdin: "ignore",
    stdout: Bun.file(join(evidence, `${label}.stdout.log`)),
    stderr: Bun.file(join(evidence, `${label}.stderr.log`)),
  });
  children.push(child);
  return child;
}
function run(command: string[]): string {
  const result = spawnSync(command[0]!, command.slice(1), {
    cwd: home,
    env,
    encoding: "utf8",
    timeout: 30_000,
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
const marker = "PHUXSURVIVAL";
const presentations: Record<string, string> = {};
function appArgs(dir: string): string[] {
  return [
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
}
function verifyCapture(label: string, expected: string): void {
  const dir = join(evidence, label);
  // Xvfb cannot confirm real-display presentation. The native receipt must
  // still prove a submitted, visible frame with matching layout generation.
  presentations[label] = assertPresentedEvidence(dir, session, true).presentation;
  const text = run(["tesseract", join(dir, "presented.png"), "stdout", "--psm", "11", "-l", "eng"]);
  writeFileSync(join(evidence, `${label}.ocr.txt`), text);
  assert(text.includes(expected), `${label}: terminal marker not rendered; OCR: ${text}`);
}
async function capture(label: string, expected: string): Promise<Bun.Subprocess> {
  const dir = join(evidence, label);
  mkdirSync(dir, { recursive: true });
  const app = launch(appArgs(dir), label);
  await until(
    `${label} submitted`,
    () => {
      assertPresentedEvidence(dir, session, true);
      return true;
    },
    app,
  );
  verifyCapture(label, expected);
  return app;
}
function serverPid(): unknown {
  const status: unknown = JSON.parse(cli("status", "--json"));
  assert(status && typeof status === "object" && "pid" in status);
  return status.pid;
}

async function nativeInput(app: Bun.Subprocess): Promise<void> {
  const window = run(["xdotool", "search", "--onlyvisible", "--pid", String(app.pid)])
    .trim()
    .split("\n")[0];
  assert(window, "Native desktop window must be mapped");
  assert(
    run(["xprop", "-id", window, "WM_CLASS"]).includes("dev.phux.desktop"),
    "Window identity must match the packaged desktop entry",
  );
  run(["xdotool", "windowactivate", "--sync", window]);
  run(["xdotool", "mousemove", "--window", window, "800", "500", "click", "1"]);
  // The observed marker does not occur in the typed command, so shell echo
  // cannot satisfy the check without actual native key delivery and Return.
  run(["xdotool", "type", "--clearmodifiers", "--delay", "30", "printf '%s%s\\n' PHUX NATIVEKEYS"]);
  run(["xdotool", "key", "Return"]);
  await until(
    "native keyboard delivery",
    () => cli("snapshot", session).includes("PHUXNATIVEKEYS"),
    app,
  );
  const clipboard = "printf '%s%s\\n' PHUX CLIPBOARD";
  const copied = spawnSync("xclip", ["-selection", "clipboard"], {
    env,
    input: clipboard,
    encoding: "utf8",
    timeout: 5000,
  });
  assert.equal(copied.status, 0, "Clipboard fixture must own the X11 selection");
  run(["xdotool", "key", "--clearmodifiers", "ctrl+shift+v"]);
  run(["xdotool", "key", "Return"]);
  await until(
    "native clipboard paste",
    () => cli("snapshot", session).includes("PHUXCLIPBOARD"),
    app,
  );
  run([
    "xdotool",
    "type",
    "--clearmodifiers",
    "--delay",
    "30",
    "printf '%s%s\\n' PHUX METAREADY; stty raw -echo; dd bs=1 count=2 2>/dev/null | od -An -tx1; stty sane",
  ]);
  run(["xdotool", "key", "Return"]);
  await until("raw Meta reader", () => cli("snapshot", session).includes("PHUXMETAREADY"), app);
  run(["xdotool", "key", "--clearmodifiers", "alt+shift+a"]);
  await until("shifted Meta bytes", () => cli("snapshot", session).includes("1b 41"), app);
}

try {
  launch(["openbox"], "window-manager");
  await until("X11 window manager", () =>
    run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"]).includes("window id"),
  );
  const server = launch(
    [phux, "--socket", socket, "server", "--session", session, "--exit-after-idle", "120"],
    "server",
  );
  await until("isolated server", () => serverPid() === server.pid, server);
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
  mkdirSync(smokeDir);
  const smoked = launch([...appArgs(smokeDir), "--smoke"], "smoke-exit");
  await until("--smoke exit", () => smoked.exitCode !== null);
  assert.equal(smoked.exitCode, 0, "--smoke must exit successfully");
  verifyCapture("smoke-exit", marker);
  let app = await capture("first-launch", marker);
  await nativeInput(app);
  for (const [signal, label] of [
    ["SIGKILL", "after-crash"],
    ["SIGTERM", "after-stop"],
  ] as const) {
    await stop(app, signal);
    assert.equal(server.exitCode, null, "App death must not kill its server");
    const afterPid = join(home, `${label}.pid`);
    cli(
      "send-keys",
      session,
      `printf '%s\\n' "$$" > '${afterPid}'; printf '\\033[2J\\033[H%s%s\\n' "$PHUX_SMOKE_VALUE" ${label.replaceAll("-", "").toUpperCase()}`,
      "Enter",
    );
    const expected = marker + label.replaceAll("-", "").toUpperCase();
    await until(
      `${label} live shell`,
      () => cli("snapshot", session).includes(expected) && existsSync(afterPid),
      server,
    );
    assert.equal(readFileSync(afterPid, "utf8").trim(), shellPid, "Original shell must survive");
    app = await capture(label, expected);
    assert.equal(serverPid(), server.pid, "Original server must survive");
  }
  const result = {
    ok: true,
    platform: `linux-${process.arch}`,
    backend: "X11 (Xvfb/lavapipe qualification; not real-GPU or Wayland qualification)",
    serverPid: server.pid,
    shellPid,
    scenarios: [
      "smoke-exit",
      "launch",
      "native keyboard",
      "clipboard paste",
      "shifted Meta bytes",
      "SIGKILL/relaunch",
      "SIGTERM/relaunch",
    ],
    presentations,
    boundary: "Submitted frame and terminal marker verified by native evidence and Tesseract OCR",
  };
  writeFileSync(join(evidence, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify(result, null, 2));
} finally {
  await cleanup();
  console.error(`Desktop evidence retained at ${evidence}; isolated state at ${home}`);
}
