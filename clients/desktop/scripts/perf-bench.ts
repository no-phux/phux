/**
 * Desktop perf bench: a fixed workload against a private server, summarized
 * per phase from the `PHUX_DESKTOP_PERF` log.
 *
 *   bun clients/desktop/scripts/perf-bench.ts [--phux <cli>] [--addon <node>]
 *     [--bundle <desktop-main.js>] [--panes 4] [--seconds 8] [--output <dir>]
 *     [--profile <phase>]
 *
 * It owns a temporary HOME, socket and server, opens the app behind the active
 * one (never activating it), lays out `--panes` terminals in one tab plus one
 * terminal in a second, background tab, then runs idle, background-tab flood,
 * one-pane flood, every-pane flood and idle-again phases. Each phase reports
 * the app's CPU, current RSS, draws, wake drains and the host's painter and
 * runtime stages. `--bundle` runs a previously built app bundle (another
 * revision's JS) instead of building this checkout's. `--profile` samples the
 * app's stacks during one phase into `<phase>.sample`. Numbers from a loaded
 * machine are observational: compare runs made back to back on one host.
 */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { prepareDesktop } from "./launch-desktop";

const desktop = resolve(import.meta.dir, "..");
const repo = resolve(desktop, "../..");
const { values } = parseArgs({
  options: {
    phux: { type: "string", default: resolve(repo, "target/debug/phux") },
    addon: {
      type: "string",
      default: resolve(desktop, ".cache/host/phux-desktop-native.darwin-arm64.node"),
    },
    bundle: { type: "string" },
    panes: { type: "string", default: "4" },
    seconds: { type: "string", default: "8" },
    output: { type: "string" },
    profile: { type: "string" },
  },
});
const phux = resolve(values.phux);
const addon = resolve(values.addon);
const panes = Number(values.panes);
const seconds = Number(values.seconds);
assert(existsSync(phux) && existsSync(addon), "CLI and addon must exist");
assert(Number.isInteger(panes) && panes >= 1 && panes <= 8, "--panes is 1-8");
assert(seconds >= 2, "--seconds is at least 2");

// Short path: Unix-domain socket paths are limited.
const home = mkdtempSync("/tmp/phux-desktop-perf-");
const evidence = values.output ? resolve(values.output) : join(home, "evidence");
mkdirSync(evidence, { recursive: true });
for (const dir of ["config", "state", "cache", "data", "run"]) mkdirSync(join(home, dir));
const socket = join(home, "run/phux.sock");
const session = "desktop-perf";
const log = join(evidence, "perf.jsonl");
const env: Record<string, string> = {
  PATH: "/usr/bin:/bin:/usr/sbin:/sbin",
  HOME: home,
  XDG_CONFIG_HOME: join(home, "config"),
  XDG_STATE_HOME: join(home, "state"),
  XDG_CACHE_HOME: join(home, "cache"),
  XDG_DATA_HOME: join(home, "data"),
  XDG_RUNTIME_DIR: join(home, "run"),
  TMPDIR: home,
  PHUX_PROFILE: "desktop-perf",
  PHUX_SOCKET: socket,
  PHUX_SESSION: session,
  PHUX_BIN: phux,
  PHUX_DESKTOP_ADDON: addon,
  PHUX_DESKTOP_BACKGROUND: "1",
  PHUX_DESKTOP_PERF: log,
  SHELL: "/bin/sh",
  TERM: "xterm-256color",
  LANG: "en_US.UTF-8",
  PS1: "$ ",
};

function cli(...args: string[]): string {
  const result = spawnSync(phux, ["--socket", socket, ...args], {
    cwd: home,
    env,
    encoding: "utf8",
    timeout: 30_000,
  });
  if (result.error || result.status !== 0)
    throw new Error(`phux ${args.join(" ")}: ${result.error?.message ?? result.stderr}`);
  return result.stdout.trim();
}

async function until(label: string, check: () => boolean): Promise<void> {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      if (check()) return;
    } catch {
      // Not yet.
    }
    await Bun.sleep(100);
  }
  throw new Error(`${label} timed out; evidence in ${evidence}`);
}

const children: Bun.Subprocess[] = [];
function launch(command: string[], name: string): Bun.Subprocess {
  const child = Bun.spawn(command, {
    cwd: home,
    env,
    stdin: "ignore",
    stdout: Bun.file(join(evidence, `${name}.stdout.log`)),
    stderr: Bun.file(join(evidence, `${name}.stderr.log`)),
  });
  children.push(child);
  return child;
}

async function stop(child: Bun.Subprocess): Promise<void> {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  const exited = await Promise.race([
    child.exited.then(() => true),
    Bun.sleep(5000).then(() => false),
  ]);
  if (!exited) {
    child.kill("SIGKILL");
    await child.exited;
  }
}

/** Where the app keeps this socket/session's layout (see start-desktop.ts). */
function layoutPath(): string {
  const target = createHash("sha256")
    .update(JSON.stringify([socket, session]))
    .digest("hex")
    .slice(0, 24);
  return join(home, "state/phux-desktop", target, "layout.json");
}

type SavedNode =
  | { kind: "leaf"; id: string; terminalId: string }
  | { kind: "split"; axis: "row" | "column"; ratio: number; first: SavedNode; second: SavedNode };

/** Balanced splits, alternating axes, over the given terminals. */
function tree(terminals: string[], depth = 0): SavedNode {
  if (terminals.length === 1) {
    const terminalId = terminals[0] ?? "";
    return { kind: "leaf", id: `bench-${terminalId}`, terminalId };
  }
  const half = Math.ceil(terminals.length / 2);
  return {
    kind: "split",
    axis: depth % 2 === 0 ? "row" : "column",
    ratio: half / terminals.length,
    first: tree(terminals.slice(0, half), depth + 1),
    second: tree(terminals.slice(half), depth + 1),
  };
}

function writeLayout(serverId: string, terminals: string[], hidden: string): void {
  const first = terminals[0] ?? "";
  writeFileSync(
    layoutPath(),
    `${JSON.stringify({
      version: 2,
      serverId,
      activeTab: "bench",
      tabs: [
        { id: "bench", root: tree(terminals), focusedTerminal: first, focusedId: `bench-${first}` },
        {
          id: "background",
          root: tree([hidden]),
          focusedTerminal: hidden,
          focusedId: `bench-${hidden}`,
        },
      ],
    })}\n`,
  );
}

function readServerId(): string | undefined {
  const saved: unknown = JSON.parse(readFileSync(layoutPath(), "utf8"));
  const id: unknown =
    saved && typeof saved === "object" ? Reflect.get(saved, "serverId") : undefined;
  return typeof id === "string" ? id : undefined;
}

interface Sample {
  at: number;
  frames?: { frames: number; p99Ms?: number; maxMs?: number };
  drains: { wakes: number; events: number; ms: number; maxMs: number };
  batches?: { batches: number; mutations: number; kinds: Record<string, number> };
  memory?: { rss: number; heapUsed: number; external: number };
  host: {
    process?: { cpu_user_us: number; cpu_system_us: number };
    metrics: {
      name: string;
      kind: string;
      value: number | { count: number; sum: number; buckets: { idx: number; count: number }[] };
    }[];
  };
}

/** A perf log line with the fields every summary reads. */
function isSample(value: unknown): value is Sample {
  if (!value || typeof value !== "object") return false;
  const host: unknown = Reflect.get(value, "host");
  return (
    typeof Reflect.get(value, "at") === "number" &&
    typeof Reflect.get(value, "drains") === "object" &&
    !!host &&
    typeof host === "object" &&
    Array.isArray(Reflect.get(host, "metrics"))
  );
}

function samples(): Sample[] {
  if (!existsSync(log)) return [];
  return readFileSync(log, "utf8")
    .split("\n")
    .filter(Boolean)
    .flatMap((line) => {
      try {
        const parsed: unknown = JSON.parse(line);
        return isSample(parsed) ? [parsed] : [];
      } catch {
        return [];
      }
    });
}

// The log-linear layout of phux-perf's Histogram: exact below 32, then eight
// sub-buckets per octave.
function bucketUpper(idx: number): number {
  if (idx < 32) return idx;
  const octave = 5 + Math.floor((idx - 32) / 8);
  const width = 2 ** (octave - 3);
  return 2 ** octave + ((idx - 32) % 8) * width + width - 1;
}

type Histo = { count: number; sum: number; buckets: Map<number, number> };

function histogram(sample: Sample, name: string): Histo | undefined {
  const metric = sample.host.metrics.find((entry) => entry.name === name);
  if (!metric || typeof metric.value !== "object") return undefined;
  return {
    count: metric.value.count,
    sum: metric.value.sum,
    buckets: new Map(metric.value.buckets.map((bucket) => [bucket.idx, bucket.count])),
  };
}

function counter(sample: Sample, name: string): number {
  const metric = sample.host.metrics.find((entry) => entry.name === name);
  return metric && typeof metric.value === "number" ? metric.value : 0;
}

function percentile(delta: Histo, p: number): number {
  const rank = Math.max(1, Math.ceil((p / 100) * delta.count));
  let seen = 0;
  for (const idx of [...delta.buckets.keys()].sort((a, b) => a - b)) {
    seen += delta.buckets.get(idx) ?? 0;
    if (seen >= rank) return bucketUpper(idx);
  }
  return 0;
}

function histogramDelta(first: Sample, last: Sample, name: string): Histo | undefined {
  const before = histogram(first, name);
  const after = histogram(last, name);
  if (!before || !after) return undefined;
  const buckets = new Map<number, number>();
  for (const [idx, count] of after.buckets) {
    const delta = count - (before.buckets.get(idx) ?? 0);
    if (delta > 0) buckets.set(idx, delta);
  }
  return { count: after.count - before.count, sum: after.sum - before.sum, buckets };
}

const STAGES = [
  "desktop.acquire",
  "desktop.prepare",
  "desktop.paint",
  "desktop.present",
  "runtime.request_wall",
  "runtime.project",
];
const COUNTERS = [
  "desktop.render",
  "desktop.acquire_rejected",
  "desktop.prepare_reused",
  "desktop.shaped",
  "runtime.publish",
  "runtime.acquire",
  "runtime.catch_up",
  "kernel.frames",
];

interface Phase {
  name: string;
  seconds: number;
  cpuPercent: number;
  rssMiB: number;
  heapMiB: number;
  drawsPerSecond: number;
  drawP99Ms: number | undefined;
  wakesPerSecond: number;
  drainMsPerSecond: number;
  batchesPerSecond: number;
  mutationsPerSecond: number;
  /** The most frequent mutation kinds, per second. */
  topMutations: Record<string, number>;
  stages: Record<string, { perSecond: number; meanUs: number; p50Us: number; p99Us: number }>;
  counters: Record<string, number>;
}

function summarize(name: string, window: Sample[]): Phase {
  const first = window[0];
  const last = window.at(-1);
  assert(first && last && window.length >= 2, `${name}: too few perf samples`);
  const elapsed = (last.at - first.at) / 1000;
  const cpu = (sample: Sample): number =>
    (sample.host.process?.cpu_user_us ?? 0) + (sample.host.process?.cpu_system_us ?? 0);
  const tail = window.slice(1);
  const stages: Phase["stages"] = {};
  for (const stage of STAGES) {
    const delta = histogramDelta(first, last, stage);
    if (!delta) continue;
    stages[stage] = {
      perSecond: Math.round(delta.count / elapsed),
      meanUs: delta.count ? Math.round(delta.sum / delta.count) : 0,
      p50Us: percentile(delta, 50),
      p99Us: percentile(delta, 99),
    };
  }
  const kinds: Record<string, number> = {};
  for (const sample of tail)
    for (const [kind, count] of Object.entries(sample.batches?.kinds ?? {}))
      kinds[kind] = (kinds[kind] ?? 0) + count;
  const topMutations = Object.fromEntries(
    Object.entries(kinds)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 6)
      .map(([kind, count]) => [kind, Math.round(count / elapsed)]),
  );
  const perSecond = (pick: (sample: Sample) => number): number =>
    Math.round(tail.reduce((sum, sample) => sum + pick(sample), 0) / elapsed);
  const counters: Record<string, number> = {};
  for (const name of COUNTERS)
    counters[name] = Math.round((counter(last, name) - counter(first, name)) / elapsed);
  return {
    name,
    seconds: Math.round(elapsed),
    cpuPercent: Math.round(((cpu(last) - cpu(first)) / 1e6 / elapsed) * 1000) / 10,
    rssMiB: Math.round(Math.max(...window.map((sample) => sample.memory?.rss ?? 0)) / 2 ** 20),
    heapMiB: Math.round(
      Math.max(...window.map((sample) => sample.memory?.heapUsed ?? 0)) / 2 ** 20,
    ),
    drawsPerSecond: Math.round(
      ((last.frames?.frames ?? 0) - (first.frames?.frames ?? 0)) / elapsed,
    ),
    drawP99Ms: last.frames?.p99Ms,
    wakesPerSecond: Math.round(
      tail.reduce((sum, sample) => sum + sample.drains.wakes, 0) / elapsed,
    ),
    drainMsPerSecond:
      Math.round((tail.reduce((sum, sample) => sum + sample.drains.ms, 0) / elapsed) * 10) / 10,
    batchesPerSecond: perSecond((sample) => sample.batches?.batches ?? 0),
    mutationsPerSecond: perSecond((sample) => sample.batches?.mutations ?? 0),
    topMutations,
    stages,
    counters,
  };
}

async function phase(name: string, results: Phase[]): Promise<void> {
  const start = Date.now();
  const profiler =
    values.profile === name && app
      ? Bun.spawn(
          [
            "/usr/bin/sample",
            String(app.pid),
            String(seconds - 1),
            "-file",
            join(evidence, `${name}.sample`),
          ],
          { stdout: "ignore", stderr: "ignore" },
        )
      : undefined;
  await Bun.sleep(seconds * 1000);
  await profiler?.exited;
  const window = samples().filter((sample) => sample.at >= start);
  const result = summarize(name, window);
  results.push(result);
  appendFileSync(join(evidence, "summary.jsonl"), `${JSON.stringify(result)}\n`);
}

function flood(terminal: string): void {
  cli(
    "send-keys",
    terminal,
    `python3 '${resolve(repo, "scripts/bench/flood.py")}' 60 full`,
    "Enter",
  );
}

function render(results: Phase[]): string {
  const lines = [
    `phase           cpu%   rssMiB heapMiB draws/s drawP99ms wakes/s drainMs/s batch/s mut/s`,
    ...results.map((phase) =>
      [
        phase.name.padEnd(14),
        String(phase.cpuPercent).padStart(6),
        String(phase.rssMiB).padStart(8),
        String(phase.heapMiB).padStart(7),
        String(phase.drawsPerSecond).padStart(7),
        String(phase.drawP99Ms?.toFixed(1) ?? "-").padStart(9),
        String(phase.wakesPerSecond).padStart(7),
        String(phase.drainMsPerSecond).padStart(9),
        String(phase.batchesPerSecond).padStart(7),
        String(phase.mutationsPerSecond).padStart(5),
      ].join(" "),
    ),
    "",
    ...results.map(
      (phase) =>
        `${phase.name.padEnd(14)} mutations/s: ${Object.entries(phase.topMutations)
          .map(([kind, count]) => `${kind} ${count}`)
          .join(", ")}`,
    ),
    "",
    `stage (per phase: n/s mean p50 p99 us)`,
  ];
  for (const stage of STAGES) {
    const cells = results.map((phase) => {
      const value = phase.stages[stage];
      return value ? `${value.perSecond}/s ${value.meanUs} ${value.p50Us} ${value.p99Us}` : "-";
    });
    lines.push(`${stage.padEnd(22)} ${cells.map((cell) => cell.padEnd(22)).join(" ")}`);
  }
  for (const name of COUNTERS) {
    const cells = results.map((phase) => `${phase.counters[name] ?? 0}/s`);
    lines.push(`${name.padEnd(22)} ${cells.map((cell) => cell.padEnd(22)).join(" ")}`);
  }
  return lines.join("\n");
}

let server: Bun.Subprocess | undefined;
let app: Bun.Subprocess | undefined;
try {
  const main = values.bundle
    ? resolve(values.bundle)
    : resolve(desktop, "dist/desktop/desktop-main.js");
  if (!values.bundle) await prepareDesktop();
  server = launch(
    [phux, "--socket", socket, "server", "--session", session, "--exit-after-idle", "120"],
    "server",
  );
  await until("server", () => cli("status", "--json").includes('"running": true'));
  const terminals = ["@1"];
  const spawn = (): string => {
    const created = /@\d+/.exec(cli("spawn", "--target", "@1", "--", "/bin/sh"))?.[0];
    assert(created, "phux spawn must print the new pane id");
    return created;
  };
  for (let index = 1; index < panes; index += 1) terminals.push(spawn());
  const hidden = spawn();
  const local = (terminal: string): string => `local:${terminal.replace(/^@/, "")}`;
  // The first launch records the daemon's identity; the bench layout reuses it.
  app = launch([process.execPath, main], "app-first");
  await until("first layout", () => readServerId() !== undefined);
  const serverId = readServerId() ?? "";
  await stop(app);
  writeLayout(serverId, terminals.map(local), local(hidden));
  app = launch([process.execPath, main], "app");
  await until("perf samples", () => samples().length >= 3);
  await Bun.sleep(2000);
  const results: Phase[] = [];
  await phase("idle", results);
  flood(hidden);
  await phase("flood-hidden", results);
  cli("send-keys", hidden, "C-c");
  await Bun.sleep(1000);
  flood(terminals[0] ?? "@1");
  await phase("flood-one", results);
  for (const terminal of terminals.slice(1)) flood(terminal);
  await phase("flood-all", results);
  for (const terminal of terminals) cli("send-keys", terminal, "C-c");
  await Bun.sleep(1000);
  await phase("idle-after", results);
  // Where resident memory sits once everything has run: the region summary
  // separates GPUI/Metal, malloc and the JS heap.
  const regions = spawnSync("/usr/bin/vmmap", ["-summary", String(app.pid)], {
    encoding: "utf8",
    timeout: 60_000,
  });
  writeFileSync(join(evidence, "vmmap.txt"), regions.stdout || regions.stderr);
  const text = render(results);
  writeFileSync(join(evidence, "summary.txt"), `${text}\n`);
  console.log(text);
  console.log(`\nevidence: ${evidence}`);
} finally {
  for (const child of [app, server]) if (child) await stop(child);
  for (const child of children) if (child.exitCode === null) child.kill("SIGKILL");
  // The sandbox holds the evidence unless --output put it elsewhere.
  if (values.output) rmSync(home, { recursive: true, force: true });
}
