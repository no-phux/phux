// Run real Chrome tests with an isolated, readiness-checked demo server.
//
// Run inside `nix develop .#browser` (or with docs/SETUP.md's browser/native
// tools). CHROMEDRIVER and CHROME select a matching local browser/driver
// pair. The standalone client's cwd, Cargo.lock, and .cargo/config.toml
// remain authoritative.
//
// The private scratch exported as TMPDIR is placed so the Unix sockets
// created under it fit in sockaddr_un. Darwin's sun_path is 104 bytes
// including NUL; the macOS default /var/folders TMPDIR is already long enough
// that nesting phux-browser-* plus mbx's cache-agent.sock overflows it.
import type { Subprocess } from "bun";
import { createHash } from "node:crypto";
import {
  accessSync,
  chmodSync,
  closeSync,
  constants as fs,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { connect as tcpConnect, type Socket } from "node:net";
import { constants, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { connect as tlsConnect } from "node:tls";
import { ROOT } from "../lib/repo.ts";
import { isTable, type Table } from "../lib/value.ts";
import { AgentFixture } from "./fixture.ts";

export type Env = Record<string, string>;

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
  "on_screen_keys_and_the_ctrl_latch_reach_the_terminal",
  // Unit tests in src/ that need a DOM and the live server.
  "exported_start_retains_live_client_until_transport_failure",
  "closed_ws_send_and_repeated_reconnect_teardown_release_every_app",
  "dropping_polled_establishment_disposes_app_reader_and_transport",
] as const;
export const AUTH_REQUIRED_TESTS = ["blackholed_wt_falls_back_to_authenticated_wss"] as const;
export const TEST_TOKEN = "11".repeat(32);

export const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

/** SIGTERM/SIGINT: unwind through every cleanup, then exit 128 + signum. */
export class Interrupted extends Error {
  constructor(readonly signum: number) {
    super(`interrupted by signal ${signum}`);
  }
}

let interrupted: Interrupted | undefined;
let wakeInterrupted: () => void = () => {};
const interruption = new Promise<void>((done) => {
  wakeInterrupted = done;
});

export function installInterruptHandlers(): void {
  for (const name of ["SIGTERM", "SIGINT"] as const) {
    process.on(name, () => {
      interrupted ??= new Interrupted(constants.signals[name]);
      wakeInterrupted();
    });
  }
}

function throwIfInterrupted(): void {
  if (interrupted) throw interrupted;
}

/** Own a loopback UDP port that silently discards every QUIC datagram. */
export class UdpBlackhole {
  packets = 0;
  private socket: Bun.udp.Socket<"buffer"> | undefined;

  get port(): number {
    if (this.socket === undefined) throw new Error("UdpBlackhole is not open");
    return this.socket.port;
  }

  get open(): boolean {
    return this.socket !== undefined && !this.socket.closed;
  }

  async start(): Promise<this> {
    this.socket = await Bun.udpSocket({
      hostname: "127.0.0.1",
      port: 0,
      socket: {
        data: () => {
          this.packets += 1;
        },
      },
    });
    return this;
  }

  close(): void {
    this.socket?.close();
  }
}

/** Signal one owned process group; false when nothing signalable is left. */
export type GroupSignal = (group: number, signal: NodeJS.Signals | 0) => boolean;

/** Signal only the session/process group that this runner created. */
export const signalGroup: GroupSignal = (group, signal) => {
  try {
    process.kill(-group, signal);
  } catch (error) {
    const code = error instanceof Error && "code" in error ? error.code : undefined;
    // Darwin answers EPERM for a group whose remaining members are zombies
    // awaiting their reaper: nothing signalable is left in it.
    if (code === "ESRCH" || code === "EPERM") return false;
    throw error;
  }
  return true;
};

/** How long an owned group gets to exit on SIGINT before it is SIGKILLed. */
export const STOP_GRACE_MS = 10_000;

export interface Owned {
  readonly pid: number;
  readonly exited: Promise<number>;
}

async function reaped(child: Owned, ms: number): Promise<void> {
  await Promise.race([child.exited, sleep(ms)]);
}

/** Drain the owned group, even when its leader exited before its children. */
export async function stop(
  child: Owned,
  graceMs = STOP_GRACE_MS,
  signal = signalGroup,
): Promise<void> {
  signal(child.pid, "SIGINT");
  // Check the whole group rather than taking the leader's exit as proof
  // that Chrome/driver/compiler descendants exited.
  const deadline = performance.now() + graceMs;
  while (performance.now() < deadline) {
    if (!signal(child.pid, 0)) {
      await reaped(child, 5000);
      return;
    }
    await sleep(100);
  }
  // A descendant can ignore SIGINT. Escalate the same owned group after the
  // bounded grace, including when the leader has already exited.
  signal(child.pid, "SIGKILL");
  await reaped(child, 5000);
}

export class CommandFailed extends Error {
  constructor(
    readonly command: string[],
    readonly code: number,
  ) {
    super(`Command '${command.join(" ")}' returned non-zero exit status ${code}.`);
  }
}

export class CommandTimeout extends Error {}

export interface RunOptions {
  readonly cwd: string;
  readonly env: Env;
  readonly timeoutMs: number;
  /** File descriptor that takes stdout and stderr. */
  readonly log: number;
  readonly graceMs?: number;
}

/** Give commands their own process group so cancellation reaps their tools. */
export async function run(command: string[], options: RunOptions): Promise<void> {
  throwIfInterrupted();
  const child = Bun.spawn(command, {
    cwd: options.cwd,
    env: options.env,
    stdin: "ignore",
    stdout: options.log,
    stderr: options.log,
    detached: true,
  });
  try {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const outcome = await Promise.race([
      child.exited,
      new Promise<"timeout">((done) => {
        timer = setTimeout(() => done("timeout"), options.timeoutMs);
      }),
      interruption.then(() => "interrupted" as const),
    ]);
    clearTimeout(timer);
    if (outcome === "timeout") {
      throw new CommandTimeout(
        `Command '${command.join(" ")}' timed out after ${options.timeoutMs}ms`,
      );
    }
    if (outcome === "interrupted") throw interrupted ?? new Interrupted(constants.signals.SIGTERM);
    if (outcome !== 0) throw new CommandFailed(command, outcome);
  } finally {
    await stop(child, options.graceMs);
  }
}

/** Run with stdout and stderr appended to a fresh log file. */
async function runLogged(command: string[], options: Omit<RunOptions, "log">, logPath: string) {
  const log = openSync(logPath, "w");
  try {
    await run(command, { ...options, log });
  } finally {
    closeSync(log);
  }
}

const LISTENING = /ws-demo-server listening on wss?:\/\/127\.0\.0\.1:(\d+)\//;

export interface Server {
  readonly exitCode: number | null;
  readonly signalCode?: string | null;
}

function exited(server: Server): boolean {
  return server.exitCode !== null || (server.signalCode ?? null) !== null;
}

/**
 * The port the demo server bound. It is started on port 0 and names the
 * address it actually bound, so no other process can take the port first.
 */
export async function boundPort(server: Server, logPath: string, pause = sleep): Promise<number> {
  for (let probe = 0; probe < 300; probe += 1) {
    if (exited(server)) throw new Error("demo server exited before reporting its bound port");
    const match = LISTENING.exec(readFileSync(logPath, "utf8"));
    if (match?.[1]) return Number(match[1]);
    await pause(100);
    throwIfInterrupted();
  }
  throw new CommandTimeout("demo server did not report its bound port after 300 bounded probes");
}

function upgradeRequest(token: string | undefined): string {
  const protocol = token ? `Sec-WebSocket-Protocol: phux.v1, phux.bearer.${token}\r\n` : "";
  return (
    "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n" +
    "Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n" +
    `Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n${protocol}\r\n`
  );
}

/** Whether a raw WebSocket upgrade on `port` is answered 101. */
export function websocketReady(port: number, tls = false, token?: string): Promise<boolean> {
  return new Promise((done, fail) => {
    const socket: Socket = tls
      ? tlsConnect({ host: "127.0.0.1", port, servername: "localhost", rejectUnauthorized: false })
      : tcpConnect({ host: "127.0.0.1", port });
    socket.setTimeout(500, () => {
      socket.destroy();
      fail(new Error("WebSocket readiness probe timed out"));
    });
    socket.once(tls ? "secureConnect" : "connect", () => socket.write(upgradeRequest(token)));
    socket.once("data", (data: Buffer) => {
      socket.destroy();
      done(data.subarray(0, 13).toString("latin1") === "HTTP/1.1 101 ");
    });
    socket.once("error", (error) => {
      socket.destroy();
      fail(error);
    });
  });
}

export type Probe = (port: number, tls: boolean, token?: string) => Promise<boolean>;

export async function waitReady(
  server: Server,
  port: number,
  {
    tls = false,
    token,
    probe = websocketReady,
    pause = sleep,
  }: {
    tls?: boolean;
    token?: string;
    probe?: Probe;
    pause?: (ms: number) => Promise<void>;
  } = {},
): Promise<void> {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (exited(server)) throw new Error("demo server exited before WebSocket readiness");
    try {
      if (await probe(port, tls, token)) return;
    } catch {
      // Not listening yet.
    }
    await pause(100);
    throwIfInterrupted();
  }
  throw new CommandTimeout("demo server WebSocket upgrade failed after 120 bounded probes");
}

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function requireBrowserTests(
  output: string,
  required: readonly string[] = REQUIRED_TESTS,
): string {
  for (const name of required) {
    if (!new RegExp(`test (?:\\S*::)?${escape(name)} \\.\\.\\. ok`).test(output)) {
      throw new Error(`Chrome did not report a passing browser test: ${name}`);
    }
  }
  return `Chrome execution verified: ${required.length} required browser tests passed`;
}

/** Merge the chosen binary into capabilities understood by wasm-bindgen. */
export function webdriverEnvironment(env: Env, directory: string, cwd: string): Env {
  const key = "WASM_BINDGEN_TEST_WEBDRIVER_JSON";
  const source = resolve(cwd, env[key] ?? "webdriver.json");
  let capabilities: Table = {};
  if (existsSync(source) || key in env) {
    const parsed: unknown = JSON.parse(readFileSync(source, "utf8"));
    if (!isTable(parsed)) throw new Error(`${source} is not a JSON object`);
    capabilities = parsed;
  }
  if (env["CHROME"]) {
    const options = capabilities["goog:chromeOptions"];
    capabilities["goog:chromeOptions"] = {
      ...(isTable(options) ? options : {}),
      binary: env["CHROME"],
    };
  }
  if (env["PHUX_TEST_ACCEPT_INSECURE_CERTS"] === "1") capabilities["acceptInsecureCerts"] = true;
  const destination = join(directory, "webdriver.json");
  writeFileSync(destination, JSON.stringify(capabilities));
  return { ...env, [key]: destination };
}

export type Runner = (
  command: string[],
  options: Omit<RunOptions, "log">,
  logPath: string,
) => Promise<void>;

export async function runChrome(
  command: string[],
  env: Env,
  logs: string,
  runner: Runner = runLogged,
) {
  const cwd = join(ROOT, "clients/phux-web");
  // Preserve user capabilities without modifying their file, and keep this
  // copy alive until wasm-bindgen has finished both browser test binaries.
  const scratch = mkdtempSync(join(env["TMPDIR"] ?? tmpdir(), "phux-webdriver-"));
  try {
    const webEnv = webdriverEnvironment(env, scratch, cwd);
    webEnv["CARGO_TARGET_DIR"] = env["PHUX_WEB_CARGO_TARGET_DIR"] ?? join(cwd, "target");
    await runner(command, { cwd, env: webEnv, timeoutMs: 600_000 }, join(logs, "chrome.log"));
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

function wasmPackTest(env: Env, tests: string[]): string[] {
  const command = ["wasm-pack", "test", "--headless", "--chrome", "--locked", ...tests];
  // wasm-pack accepts a driver; wasm-bindgen needs capabilities for the binary.
  if (env["CHROMEDRIVER"]) command.splice(4, 0, "--chromedriver", env["CHROMEDRIVER"]);
  return command;
}

/** The demo server in a group of its own, logging to `logPath`, stopped after `body`. */
async function withDemoServer<T>(
  env: Env,
  logPath: string,
  body: (server: Subprocess) => Promise<T>,
): Promise<T> {
  throwIfInterrupted();
  const log = openSync(logPath, "w");
  const server = Bun.spawn([join(env["CARGO_TARGET_DIR"] ?? "", "debug/examples/ws_demo_server")], {
    cwd: ROOT,
    env,
    stdin: "ignore",
    stdout: log,
    stderr: log,
    detached: true,
  });
  try {
    return await body(server);
  } finally {
    await stop(server);
    closeSync(log);
  }
}

async function browserTests(env: Env, logs: string): Promise<void> {
  env["PHUX_WS_ADDR"] = "127.0.0.1:0";
  // A `cat` pane: each typed line comes back as program output, escape
  // sequences included (ws_demo_server's default pane is a shell).
  env["PHUX_DEMO_PANE"] = "cat";
  const command = wasmPackTest(env, [
    "--lib",
    "--test",
    "render",
    "--test",
    "e2e_browser",
    "--test",
    "e2e_input",
  ]);
  await withDemoServer(env, join(logs, "server.log"), async (server) => {
    const port = await boundPort(server, join(logs, "server.log"));
    env["PHUX_TEST_WS_URL"] = `ws://127.0.0.1:${port}/`;
    await waitReady(server, port);
    // Do not reuse the native target or override the standalone rustflags.
    const socketPath = join(env["TMPDIR"] ?? tmpdir(), `phux-e2e-${server.pid}.sock`);
    const binary = join(env["CARGO_TARGET_DIR"] ?? "", "debug/phux");
    const fixture = new AgentFixture(binary, socketPath, env).start();
    try {
      env["PHUX_TEST_AGENT_URL"] = fixture.url;
      await runChrome(command, env, logs);
      console.log(requireBrowserTests(readFileSync(join(logs, "chrome.log"), "utf8")));
      // The authenticated fallback reuses chrome.log; retain proof of the
      // main browser scenarios, including live agent discovery.
      copyFileSync(join(logs, "chrome.log"), join(logs, "browser.log"));
    } finally {
      await fixture.close();
    }
  });
}

export function writeAuthFixture(directory: string): { cert: string; key: string; tokens: string } {
  const cert = join(directory, "cert.pem");
  const key = join(directory, "key.pem");
  const tokens = join(directory, "tokens.json");
  const openssl = Bun.spawnSync(
    [
      "openssl",
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      key,
      "-out",
      cert,
      "-days",
      "1",
      "-subj",
      "/CN=localhost",
      "-addext",
      "subjectAltName=DNS:localhost,IP:127.0.0.1",
    ],
    { stdout: "ignore", stderr: "ignore", timeout: 30_000 },
  );
  if (openssl.exitCode !== 0) throw new Error(`openssl exited ${openssl.exitCode}`);
  const verifier = createHash("sha256").update(Buffer.from(TEST_TOKEN, "hex")).digest("hex");
  writeFileSync(
    tokens,
    JSON.stringify({
      version: 1,
      credentials: [
        {
          id: "browser-e2e",
          verifier: `sha256:${verifier}`,
          principal: "device:browser-e2e",
          scopes: ["terminal.control"],
          issued_at: "2026-09-13T00:00:00Z",
          expires_at: null,
          revoked_at: null,
          generation: 1,
        },
      ],
    }),
  );
  chmodSync(key, 0o600);
  chmodSync(tokens, 0o600);
  return { cert, key, tokens };
}

async function authenticatedFallbackTests(
  env: Env,
  logs: string,
  directory: string,
): Promise<void> {
  const { cert, key, tokens } = writeAuthFixture(directory);
  const authEnv: Env = {
    ...env,
    PHUX_WS_ADDR: "127.0.0.1:0",
    PHUX_WS_SECURE: "1",
    PHUX_WS_TLS_CERT: cert,
    PHUX_WS_TLS_KEY: key,
    PHUX_WS_TOKENS: tokens,
    PHUX_TEST_TOKEN: TEST_TOKEN,
    PHUX_TEST_ACCEPT_INSECURE_CERTS: "1",
  };
  const blackhole = await new UdpBlackhole().start();
  try {
    authEnv["PHUX_TEST_WT_URL"] = `https://127.0.0.1:${blackhole.port}/session?token=${TEST_TOKEN}`;
    const command = wasmPackTest(authEnv, ["--test", "e2e_fallback_auth"]);
    const logPath = join(logs, "auth-server.log");
    await withDemoServer(authEnv, logPath, async (server) => {
      const port = await boundPort(server, logPath);
      authEnv["PHUX_TEST_WSS_URL"] = `wss://127.0.0.1:${port}/`;
      await waitReady(server, port, { tls: true, token: TEST_TOKEN });
      await runChrome(command, authEnv, logs);
      const chrome = readFileSync(join(logs, "chrome.log"), "utf8");
      console.log(requireBrowserTests(chrome, AUTH_REQUIRED_TESTS));
      if (blackhole.packets === 0) {
        throw new Error("Chrome sent no datagrams to the WebTransport blackhole");
      }
      console.log(`WebTransport blackhole verified: ${blackhole.packets} datagrams discarded`);
    });
  } finally {
    blackhole.close();
  }
}

/** The runner's own knobs; every other inherited PHUX_* variable is dropped. */
export const RUNNER_VARIABLES = ["PHUX_WEB_CARGO_TARGET_DIR", "PHUX_BROWSER_AUTH_ONLY"];

/**
 * The runner's environment without inherited phux configuration.
 *
 * Run from a phux pane, the shell carries the production server's socket,
 * listener addresses, TLS material, and token store (PHUX_SOCKET,
 * PHUX_QUIC_ADDR, PHUX_WS_TOKENS, ...). The demo server snapshots PHUX_* at
 * startup, so an inherited value would aim it at production state.
 */
export function isolatedEnvironment(environ: Record<string, string | undefined>): Env {
  const env: Env = {};
  for (const [name, value] of Object.entries(environ)) {
    if (value === undefined) continue;
    if (!name.startsWith("PHUX_") || RUNNER_VARIABLES.includes(name)) env[name] = value;
  }
  return env;
}

/**
 * Compile the throwaway native harness at opt-level 0.
 *
 * The workspace dev profile is opt-level 1. Run 38457853567 spent 7m 00s in
 * this one cargo build; the browser checks after it were seconds. Keep the
 * override off the caller's env so wasm-pack still matches the artifacts
 * earlier steps in the job already built.
 */
export function harnessBuildEnv(env: Env): Env {
  return { ...env, CARGO_PROFILE_DEV_OPT_LEVEL: "0" };
}

// Usable Unix socket path length: sun_path minus its trailing NUL. Darwin's
// sun_path is 104 bytes; Linux's is 108. Same split as phux-server's
// MAX_SOCKET_PATH_LEN (103 on Darwin, 107 on Linux).
export const DARWIN_SOCKET_PATH_MAX = 103;
export const LINUX_SOCKET_PATH_MAX = 107;

// mkdtemp's random suffix is six characters; budget eight, which also
// covers CPython's tempfile names in tools nested under the scratch.
export const TEMP_NAME_RANDOM = 8;
export const SCRATCH_PREFIX = "phux-browser-";

// Relative to the private scratch once it is exported as TMPDIR. mbx is
// external and binds cache-agent.sock inside mbx-session-*; the session
// token in this representative is eight characters. pid_t is 32-bit, so a
// decimal pid is at most ten digits. The webdriver entry is the scratch
// directory this runner creates beside those sockets, not a socket name of
// its own.
export const MBX_SOCKET_SUFFIX = "mbx-session-XXXXXXXX/cache-agent.sock";
export const E2E_SOCKET_SUFFIX = `phux-e2e-${"0".repeat(10)}.sock`;
export const WEBDRIVER_SCRATCH_SUFFIX = `phux-webdriver-${"x".repeat(TEMP_NAME_RANDOM)}`;
export const NESTED_SOCKET_SUFFIXES = [
  MBX_SOCKET_SUFFIX,
  E2E_SOCKET_SUFFIX,
  WEBDRIVER_SCRATCH_SUFFIX,
];

// Portable short directories, none of them user-specific. The first that
// exists, is writable, and still leaves socket headroom is the parent of the
// private scratch. /tmp stays under the Darwin limit even after
// phux-browser-* and mbx's nested socket; /private/tmp is its macOS target.
export const SHORT_SCRATCH_BASES = ["/tmp", "/private/tmp", "/var/tmp"];

/** Usable sockaddr_un path bytes on `platform`, excluding the trailing NUL. */
export function socketPathMax(platform: string): number {
  return platform === "linux" ? LINUX_SOCKET_PATH_MAX : DARWIN_SOCKET_PATH_MAX;
}

const bytes = (path: string) => Buffer.byteLength(path);

function longestSocket(base: string): string {
  const scratch = join(base, SCRATCH_PREFIX + "x".repeat(TEMP_NAME_RANDOM));
  const paths = NESTED_SOCKET_SUFFIXES.map((suffix) => join(scratch, suffix));
  return paths.reduce((longest, path) => (bytes(path) > bytes(longest) ? path : longest));
}

/** True when a private scratch under `base` cannot hold every nested socket. */
export function scratchSocketOverflow(base: string, limit: number): boolean {
  return bytes(longestSocket(base)) > limit;
}

function writableDir(path: string): boolean {
  try {
    if (!statSync(path).isDirectory()) return false;
    accessSync(path, fs.W_OK | fs.X_OK);
    return true;
  } catch {
    return false;
  }
}

const HOSTS: Record<string, string> = { darwin: "Darwin", linux: "Linux" };

function scratchLimitMessage(
  preferred: string,
  limit: number,
  platform: string,
  candidates: readonly string[],
) {
  const example = longestSocket(preferred);
  const host = HOSTS[platform] ?? platform;
  return (
    `browser gate TMPDIR ${preferred} would make ${example} ${bytes(example)} bytes, ` +
    `past the ${limit}-byte ${host} Unix socket path limit. ` +
    `None of ${candidates.join(", ")} is a writable directory short enough. ` +
    "Set TMPDIR to a short writable directory such as /tmp and retry."
  );
}

/**
 * Parent for the private per-run scratch that child processes see as TMPDIR.
 *
 * A caller-supplied TMPDIR that already leaves room for the nested sockets is
 * used unchanged. A long default (macOS /var/folders/...) is not: mbx then
 * creates <scratch>/mbx-session-*\/cache-agent.sock and Darwin rejects the
 * bind. When no short writable base exists, fail here so the limit is named
 * instead of surfacing later as an opaque compile error.
 */
export function browserScratchBase(
  environ: Env,
  {
    platform = process.platform,
    candidates = SHORT_SCRATCH_BASES,
  }: {
    platform?: string;
    candidates?: readonly string[];
  } = {},
): string {
  const limit = socketPathMax(platform);
  const explicit = environ["TMPDIR"];
  if (explicit && !scratchSocketOverflow(explicit, limit)) return explicit;
  const preferred = explicit || tmpdir();
  if (!explicit && !scratchSocketOverflow(preferred, limit)) return preferred;
  for (const candidate of candidates) {
    if (!scratchSocketOverflow(candidate, limit) && writableDir(candidate)) return candidate;
  }
  throw new Error(scratchLimitMessage(preferred, limit, platform, candidates));
}

/** A private scratch under `base`; the caller removes it. */
export function browserScratchDir(base: string): string {
  return mkdtempSync(join(base, SCRATCH_PREFIX));
}

const LOGS = ["build.log", "chrome.log", "browser.log", "server.log", "auth-server.log"];

export async function webBrowser(): Promise<number> {
  installInterruptHandlers();
  const env = isolatedEnvironment(process.env);
  for (const [variable, binary] of [
    ["CHROME", "chromium"],
    ["CHROMEDRIVER", "chromedriver"],
  ] as const) {
    const executable = Bun.which(binary);
    if (executable && env[variable] === undefined) env[variable] = executable;
  }
  env["CARGO_TARGET_DIR"] = resolve(env["CARGO_TARGET_DIR"] ?? join(ROOT, "target/web-native"));
  // Choose the scratch parent before touching logs or compiling. The native
  // build keeps the caller's TMPDIR; the private scratch is exported only for
  // the browser run, which is where mbx nests cache-agent.sock under it.
  const scratchParent = browserScratchBase(env);
  const logs = join(ROOT, "target/web-browser-logs");
  mkdirSync(logs, { recursive: true });
  for (const name of LOGS) rmSync(join(logs, name), { force: true });
  try {
    // One Cargo schedule for the demo server and the phux binary. Split
    // invocations on 2 jobs were 5m 55s then 3m 41s (run 38451988979). One
    // schedule at the runner's 4 cores was 7m 00s (run 38457853567). Cargo's
    // default job count is the machine's CPU count.
    await runLogged(
      [
        "cargo",
        "build",
        "--locked",
        "-p",
        "phux-server",
        "--example",
        "ws_demo_server",
        "-p",
        "phux",
        "--bin",
        "phux",
      ],
      { cwd: ROOT, env: harnessBuildEnv(env), timeoutMs: 1_200_000 },
      join(logs, "build.log"),
    );
    const scratch = browserScratchDir(scratchParent);
    try {
      env["TMPDIR"] = scratch;
      if (env["PHUX_BROWSER_AUTH_ONLY"] !== "1") await browserTests(env, logs);
      await authenticatedFallbackTests(env, logs, scratch);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
    return 0;
  } catch (error) {
    if (error instanceof Interrupted) return 128 + error.signum;
    throw error;
  } finally {
    for (const name of readdirSync(logs)
      .filter((file) => file.endsWith(".log"))
      .sort()) {
      console.log(`\n${join(logs, name)}:\n${readFileSync(join(logs, name), "utf8")}`);
    }
  }
}
