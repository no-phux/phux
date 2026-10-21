// Run real Chrome tests against an isolated, readiness-checked demo server.
// Run inside `nix develop .#browser`; CHROME and CHROMEDRIVER override the
// browser/driver pair found on PATH.
import type { Subprocess } from "bun";
import { createHash } from "node:crypto";
import {
  accessSync,
  chmodSync,
  closeSync,
  constants as fs,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { connect as tcpConnect } from "node:net";
import { constants, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { connect as tlsConnect } from "node:tls";
import { Failure } from "../lib/command.ts";
import { capture } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import { AgentFixture } from "./fixture.ts";

type Env = Record<string, string>;

export const REQUIRED_TESTS = [
  "renders_engine_grid_to_canvas",
  "a_wide_character_paints_across_its_spacer_cell",
  "exact_wasm_codec_selects_native_and_renders_live_server",
  "agents_started_after_browser_attach_update_badges_without_reloading",
  "synthesized_only_browser_remains_compatible_with_native_server",
  "keys_ime_commits_and_paste_reach_the_terminal_and_nothing_else_is_captured",
  "resize_reflows_the_live_terminal_and_canvas",
  "scrollback_pages_by_wheel_and_shift_page_up_and_a_drag_copies_from_it",
  "find_highlights_matches_in_history_and_steps_between_them",
  "mouse_reports_reach_a_tracking_program_and_shift_drag_still_selects",
  "modifier_click_opens_http_links_in_a_new_tab_and_nothing_else",
  "the_bell_announces_itself_and_flashes_the_canvas",
  "a_double_width_character_copies_without_a_trailing_space",
  "focus_changes_reach_a_program_that_asks_for_them",
  "the_wheel_on_the_alternate_screen_is_arrow_keys",
  "mouse_positions_land_on_their_cells_after_another_client_reports_other_cells",
  "a_finger_scrolls_scrollback_a_hold_selects_and_a_tap_opens_a_link",
  "a_hold_still_selects_scrollback_when_another_cell_size_detaches",
  "on_screen_keys_and_the_ctrl_latch_reach_the_terminal",
  // Unit tests in src/ that need a DOM and the live server.
  "exported_start_retains_live_client_until_transport_failure",
  "closed_ws_send_and_repeated_reconnect_teardown_release_every_app",
  "a_version_refusal_marks_the_canvas_and_never_redials",
  "dropping_polled_establishment_disposes_app_reader_and_transport",
] as const;
const AUTH_REQUIRED_TESTS = ["blackholed_wt_falls_back_to_authenticated_wss"];
const TEST_TOKEN = "11".repeat(32);

/** SIGTERM/SIGINT: unwind through every cleanup, then exit 128 + signum. */
class Interrupted extends Error {
  constructor(readonly signum: number) {
    super(`interrupted by signal ${signum}`);
  }
}

let interrupted: Interrupted | undefined;
const interruption = Promise.withResolvers<"interrupted">();

function throwIfInterrupted(): void {
  if (interrupted) throw interrupted;
}

/** Signal the process group this runner created; false when nothing signalable is left. */
export function signalGroup(group: number, signal: NodeJS.Signals | 0): boolean {
  try {
    process.kill(-group, signal);
    return true;
  } catch (error) {
    const code = error instanceof Error && "code" in error ? error.code : undefined;
    // Darwin answers EPERM for a group holding only zombies awaiting their reaper.
    if (code === "ESRCH" || code === "EPERM") return false;
    throw error;
  }
}

/**
 * SIGINT the owned group, then SIGKILL it after `graceMs`. The whole group is
 * checked, not the leader: Chrome, driver and compiler descendants can outlive
 * it or ignore SIGINT.
 */
async function stop(child: Subprocess, graceMs = 10_000): Promise<void> {
  signalGroup(child.pid, "SIGINT");
  const deadline = performance.now() + graceMs;
  while (signalGroup(child.pid, 0)) {
    if (performance.now() >= deadline) {
      signalGroup(child.pid, "SIGKILL");
      break;
    }
    await Bun.sleep(100);
  }
  await Promise.race([child.exited, Bun.sleep(5000)]);
}

interface Spawn {
  cwd: string;
  env: Env;
  /** Truncated, then takes the child's stdout and stderr. */
  logPath: string;
  graceMs?: number;
}

/** Run `body` with `argv` alive in its own process group, then stop the group. */
async function owned<T>(
  argv: string[],
  { cwd, env, logPath, graceMs }: Spawn,
  body: (child: Subprocess) => Promise<T>,
): Promise<T> {
  throwIfInterrupted();
  const log = openSync(logPath, "w");
  const child = Bun.spawn(argv, {
    cwd,
    env,
    stdin: "ignore",
    stdout: log,
    stderr: log,
    detached: true,
  });
  try {
    return await body(child);
  } finally {
    await stop(child, graceMs);
    closeSync(log);
  }
}

/** Run `argv` to completion; a non-zero exit, timeout or interrupt throws after the group is stopped. */
export function run(argv: string[], options: Spawn & { timeoutMs: number }): Promise<void> {
  return owned(argv, options, async (child) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<"timeout">((done) => {
      timer = setTimeout(() => done("timeout"), options.timeoutMs);
    });
    const outcome = await Promise.race([child.exited, timeout, interruption.promise]);
    clearTimeout(timer);
    throwIfInterrupted();
    const command = argv.join(" ");
    if (outcome === "timeout")
      throw new Failure(`${command} timed out after ${options.timeoutMs}ms`);
    if (outcome !== 0) throw new Failure(`${command} exited ${outcome}`);
  });
}

export type Server = Pick<Subprocess, "exitCode" | "signalCode">;

/** Retry `check` every 100ms until it yields a value, failing once the server exits or `attempts` run out. */
export async function poll<T>(
  server: Server,
  what: string,
  attempts: number,
  check: () => Promise<T | undefined>,
  pause: (ms: number) => Promise<void> = Bun.sleep,
): Promise<T> {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    if (server.exitCode !== null || server.signalCode !== null) {
      throw new Failure(`demo server exited before ${what}`);
    }
    const value = await check().catch(() => undefined);
    if (value !== undefined) return value;
    await pause(100);
    throwIfInterrupted();
  }
  throw new Failure(`demo server gave no ${what} after ${attempts} bounded probes`);
}

const LISTENING = /ws-demo-server listening on wss?:\/\/127\.0\.0\.1:(\d+)\//;

/** The port the demo server bound; it starts on port 0 so nothing can take the port first. */
export function boundPort(server: Server, logPath: string): Promise<number> {
  return poll(server, "its bound port", 300, async () => {
    const port = LISTENING.exec(readFileSync(logPath, "utf8"))?.[1];
    return port === undefined ? undefined : Number(port);
  });
}

/** Whether a raw WebSocket upgrade on `port` is answered 101. */
export function websocketReady(port: number, tls = false, token?: string): Promise<boolean> {
  const protocol = token ? `Sec-WebSocket-Protocol: phux.v1, phux.bearer.${token}\r\n` : "";
  const request =
    "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
    `Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n${protocol}\r\n`;
  return new Promise((done, fail) => {
    const socket = tls
      ? tlsConnect({ host: "127.0.0.1", port, servername: "localhost", rejectUnauthorized: false })
      : tcpConnect({ host: "127.0.0.1", port });
    socket.setTimeout(500, () => socket.destroy(new Error("WebSocket readiness probe timed out")));
    socket.once(tls ? "secureConnect" : "connect", () => socket.write(request));
    socket.once("data", (data: Buffer) => {
      socket.destroy();
      done(data.subarray(0, 13).toString("latin1") === "HTTP/1.1 101 ");
    });
    socket.once("error", fail);
    socket.once("close", () => done(false));
  });
}

function waitReady(server: Server, port: number, tls: boolean, token?: string): Promise<true> {
  return poll(server, "WebSocket readiness", 120, async () =>
    (await websocketReady(port, tls, token)) ? true : undefined,
  );
}

/** Fail unless the Chrome log reports every `required` test as passing. */
export function requireBrowserTests(output: string, required: readonly string[]): string {
  for (const name of required) {
    if (!new RegExp(`test (?:\\S*::)?${name} \\.\\.\\. ok`).test(output)) {
      throw new Failure(`Chrome did not report a passing browser test: ${name}`);
    }
  }
  return `Chrome execution verified: ${required.length} required browser tests passed`;
}

/** Write the wasm-bindgen WebDriver capabilities for the chosen Chrome; returns the path. */
export function webdriverCapabilities(
  directory: string,
  chrome: string | undefined,
  insecure: boolean,
): string {
  const path = join(directory, "webdriver.json");
  const capabilities = {
    ...(chrome ? { "goog:chromeOptions": { binary: chrome } } : {}),
    ...(insecure ? { acceptInsecureCerts: true } : {}),
  };
  writeFileSync(path, JSON.stringify(capabilities));
  return path;
}

interface Paths {
  logs: string;
  /** The private scratch exported as TMPDIR. */
  scratch: string;
  /** The native CARGO_TARGET_DIR holding the demo server and phux. */
  target: string;
}

/** wasm-pack the standalone client's `tests` in Chrome and check `required` passed. */
async function chrome(
  env: Env,
  paths: Paths,
  tests: string[],
  required: readonly string[],
  insecure = false,
): Promise<void> {
  const cwd = join(ROOT, "clients/phux-web");
  const driver = env["CHROMEDRIVER"] ? ["--chromedriver", env["CHROMEDRIVER"]] : [];
  const logPath = join(paths.logs, "chrome.log");
  // The standalone client keeps its own target, Cargo.lock and rustflags.
  const webEnv = {
    ...env,
    CARGO_TARGET_DIR: join(cwd, "target"),
    WASM_BINDGEN_TEST_WEBDRIVER_JSON: webdriverCapabilities(paths.scratch, env["CHROME"], insecure),
  };
  const command = ["wasm-pack", "test", "--headless", "--chrome", ...driver, "--locked", ...tests];
  await run(command, { cwd, env: webEnv, logPath, timeoutMs: 600_000 });
  console.log(requireBrowserTests(readFileSync(logPath, "utf8"), required));
}

/** The demo server in its own group, ready on its bound port for `body`. */
function demoServer(
  env: Env,
  paths: Paths,
  log: string,
  body: (server: Subprocess, port: number) => Promise<void>,
): Promise<void> {
  const logPath = join(paths.logs, log);
  const binary = join(paths.target, "debug/examples/ws_demo_server");
  return owned([binary], { cwd: ROOT, env, logPath }, async (server) =>
    body(server, await boundPort(server, logPath)),
  );
}

const BROWSER_SUITES = "--lib --test render --test e2e_browser --test e2e_input".split(" ");
// A `cat` pane echoes each typed line, escape sequences included.
const DEMO_SERVER = { PHUX_WS_ADDR: "127.0.0.1:0", PHUX_DEMO_PANE: "cat" };

async function browserTests(env: Env, paths: Paths): Promise<void> {
  const serverEnv = { ...env, ...DEMO_SERVER };
  await demoServer(serverEnv, paths, "server.log", async (server, port) => {
    await waitReady(server, port, false);
    // The demo server's owner socket, in the TMPDIR it inherited.
    const socket = join(paths.scratch, `phux-e2e-${server.pid}.sock`);
    const fixture = new AgentFixture(join(paths.target, "debug/phux"), socket, serverEnv);
    try {
      const testEnv = {
        ...serverEnv,
        PHUX_TEST_WS_URL: `ws://127.0.0.1:${port}/`,
        PHUX_TEST_AGENT_URL: fixture.url,
      };
      await chrome(testEnv, paths, BROWSER_SUITES, REQUIRED_TESTS);
      // The fallback run overwrites chrome.log; keep the main run's proof.
      copyFileSync(join(paths.logs, "chrome.log"), join(paths.logs, "browser.log"));
    } finally {
      await fixture.close();
    }
  });
}

/** A localhost certificate and a token store holding TEST_TOKEN, as demo-server env. */
function authEnvironment(directory: string): Env {
  const cert = join(directory, "cert.pem");
  const key = join(directory, "key.pem");
  const tokens = join(directory, "tokens.json");
  capture(
    "openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=localhost"
      .split(" ")
      .concat("-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1", "-keyout", key, "-out", cert),
    { timeoutMs: 30_000 },
  );
  const verifier = createHash("sha256").update(Buffer.from(TEST_TOKEN, "hex")).digest("hex");
  const credential = {
    id: "browser-e2e",
    verifier: `sha256:${verifier}`,
    principal: "device:browser-e2e",
    scopes: ["terminal.control"],
    issued_at: "2026-09-13T00:00:00Z",
    expires_at: null,
    revoked_at: null,
    generation: 1,
  };
  writeFileSync(tokens, JSON.stringify({ version: 1, credentials: [credential] }));
  chmodSync(key, 0o600);
  chmodSync(tokens, 0o600);
  return {
    PHUX_WS_SECURE: "1",
    PHUX_WS_TLS_CERT: cert,
    PHUX_WS_TLS_KEY: key,
    PHUX_WS_TOKENS: tokens,
  };
}

/** Bind a loopback UDP port that counts and discards every datagram. */
export async function udpBlackhole() {
  let packets = 0;
  const socket = await Bun.udpSocket({
    hostname: "127.0.0.1",
    port: 0,
    socket: {
      data: () => {
        packets += 1;
      },
    },
  });
  return { port: socket.port, packets: () => packets, close: () => socket.close() };
}

/** A blackholed WebTransport attempt must fall back to authenticated WSS. */
async function fallbackTests(env: Env, paths: Paths): Promise<void> {
  const serverEnv = { ...env, ...DEMO_SERVER, ...authEnvironment(paths.scratch) };
  const blackhole = await udpBlackhole();
  try {
    await demoServer(serverEnv, paths, "auth-server.log", async (server, port) => {
      await waitReady(server, port, true, TEST_TOKEN);
      const testEnv = {
        ...serverEnv,
        PHUX_TEST_TOKEN: TEST_TOKEN,
        PHUX_TEST_WT_URL: `https://127.0.0.1:${blackhole.port}/session?token=${TEST_TOKEN}`,
        PHUX_TEST_WSS_URL: `wss://127.0.0.1:${port}/`,
      };
      await chrome(testEnv, paths, ["--test", "e2e_fallback_auth"], AUTH_REQUIRED_TESTS, true);
    });
    if (blackhole.packets() === 0) {
      throw new Failure("Chrome sent no datagrams to the WebTransport blackhole");
    }
    console.log(`WebTransport blackhole verified: ${blackhole.packets()} datagrams discarded`);
  } finally {
    blackhole.close();
  }
}

/**
 * The environment without inherited phux configuration. From a phux pane the
 * shell carries the production server's socket, listeners, TLS material and
 * token store, and the demo server snapshots PHUX_* at startup.
 */
export function isolatedEnvironment(environ: Record<string, string | undefined>): Env {
  return Object.fromEntries(
    Object.entries(environ).filter(
      (entry): entry is [string, string] => entry[1] !== undefined && !entry[0].startsWith("PHUX_"),
    ),
  );
}

// The longest Unix socket under the scratch parent: mbx (external) binds
// cache-agent.sock under the private scratch, whose mkdtemp suffix is budgeted
// at eight characters. The demo server's phux-e2e-<pid>.sock is shorter.
const NESTED_SOCKET = "phux-browser-XXXXXXXX/mbx-session-XXXXXXXX/cache-agent.sock";
const SHORT_SCRATCH_BASES = ["/tmp", "/private/tmp", "/var/tmp"];

/** Usable sockaddr_un path bytes, excluding the NUL: Darwin's sun_path is 104, Linux's 108. */
export function socketPathMax(platform: string): number {
  return platform === "linux" ? 107 : 103;
}

function writableDir(path: string): boolean {
  try {
    accessSync(path, fs.W_OK | fs.X_OK);
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Parent for the private scratch child processes see as TMPDIR: TMPDIR when
 * its nested sockets fit sockaddr_un, else the first short writable base. A
 * long macOS /var/folders TMPDIR otherwise fails mbx's bind deep in the build.
 */
export function scratchBase(
  tmp: string | undefined,
  platform: string = process.platform,
  candidates: readonly string[] = SHORT_SCRATCH_BASES,
): string {
  const limit = socketPathMax(platform);
  const fits = (base: string) => Buffer.byteLength(join(base, NESTED_SOCKET)) <= limit;
  const preferred = tmp || tmpdir();
  if (fits(preferred)) return preferred;
  const short = candidates.find((base) => fits(base) && writableDir(base));
  if (short !== undefined) return short;
  const example = join(preferred, NESTED_SOCKET);
  throw new Failure(
    `browser gate TMPDIR ${preferred} would make ${example} ${Buffer.byteLength(example)} bytes, ` +
      `past the ${limit}-byte ${platform} Unix socket path limit, and none of ` +
      `${candidates.join(", ")} is a short writable directory. ` +
      "Set TMPDIR to a short writable directory such as /tmp and retry.",
  );
}

const LOGS = ["build.log", "chrome.log", "browser.log", "server.log", "auth-server.log"];

export async function webBrowser(): Promise<number> {
  for (const name of ["SIGTERM", "SIGINT"] as const) {
    process.on(name, () => {
      interrupted ??= new Interrupted(constants.signals[name]);
      interruption.resolve("interrupted");
    });
  }
  const env = isolatedEnvironment(process.env);
  for (const [variable, binary] of Object.entries({
    CHROME: "chromium",
    CHROMEDRIVER: "chromedriver",
  })) {
    const found = env[variable] ?? Bun.which(binary);
    if (found) env[variable] = found;
  }
  const target = resolve(env["CARGO_TARGET_DIR"] ?? join(ROOT, "target/web-native"));
  env["CARGO_TARGET_DIR"] = target;
  const scratchParent = scratchBase(env["TMPDIR"]);
  const logs = join(ROOT, "target/web-browser-logs");
  mkdirSync(logs, { recursive: true });
  for (const name of LOGS) rmSync(join(logs, name), { force: true });
  try {
    // One Cargo schedule for both binaries, at opt-level 0: the dev profile's
    // opt-level 1 made this throwaway build the job's slowest step.
    await run(
      "cargo build --locked -p phux-server --example ws_demo_server -p phux --bin phux".split(" "),
      {
        cwd: ROOT,
        env: { ...env, CARGO_PROFILE_DEV_OPT_LEVEL: "0" },
        logPath: join(logs, "build.log"),
        timeoutMs: 1_200_000,
      },
    );
    const scratch = mkdtempSync(join(scratchParent, "phux-browser-"));
    try {
      const paths = { logs, scratch, target };
      await browserTests({ ...env, TMPDIR: scratch }, paths);
      await fallbackTests({ ...env, TMPDIR: scratch }, paths);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
    return 0;
  } catch (error) {
    if (error instanceof Interrupted) return 128 + error.signum;
    throw error;
  } finally {
    for (const name of readdirSync(logs).sort()) {
      if (name.endsWith(".log"))
        console.log(`\n${join(logs, name)}:\n${readFileSync(join(logs, name), "utf8")}`);
    }
  }
}
