// Bounded shutdown for an invocation whose descendants may leave its process
// group: cargo-mutants starts Cargo and tests in groups of their own.
import type { Subprocess } from "bun";
import { ProcessError } from "../lib/process.ts";

/** pid -> (parent pid, start time). */
export type Snapshot = Map<number, { parent: number; birth: string }>;

export function parseSnapshot(text: string): Snapshot {
  const processes: Snapshot = new Map();
  for (const line of text.split("\n")) {
    const [, pid, parent, birth] = /^\s*(\d+)\s+(\d+)\s+(.+?)\s*$/.exec(line) ?? [];
    if (birth) processes.set(Number(pid), { parent: Number(parent), birth });
  }
  return processes;
}

/** Start times keep a reused PID from being mistaken for an owned process. */
async function snapshot(): Promise<Snapshot> {
  const argv = ["ps", "-axo", "pid=,ppid=,lstart="];
  // Busy macOS hosts can need more than a second to enumerate processes.
  // Keep discovery bounded without blocking child reaping or signal handlers.
  const child = Bun.spawn(argv, {
    timeout: 5000,
    killSignal: "SIGKILL",
    stdio: ["ignore", "pipe", "pipe"],
  });
  const [code, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);
  if (code !== 0) {
    throw new ProcessError(argv, stderr || `exit ${code} (signal: ${String(child.signalCode)})`);
  }
  return parseSnapshot(stdout);
}

/** Owned PIDs still running as the process they were when first seen. */
export function living(owned: Map<number, string>, processes: Snapshot): Set<number> {
  return new Set(
    [...owned].filter(([pid, birth]) => processes.get(pid)?.birth === birth).map(([pid]) => pid),
  );
}

/** Adopt descendants of living owned processes; identities outlive reparenting. */
export function trackDescendants(owned: Map<number, string>, processes: Snapshot): void {
  for (;;) {
    const parents = living(owned, processes);
    const children = [...processes].filter(
      ([pid, { parent }]) => parents.has(parent) && !owned.has(pid),
    );
    if (children.length === 0) return;
    for (const [pid, { birth }] of children) owned.set(pid, birth);
  }
}

function signal(pid: number, name: NodeJS.Signals): void {
  try {
    process.kill(pid, name);
  } catch (error) {
    if (!(error instanceof Error && "code" in error && error.code === "ESRCH")) throw error;
  }
}

export interface Cleanup {
  tracked_pids: number[];
  escalated_pids: number[];
}

const ascending = (left: number, right: number) => left - right;

/** SIGINT `child` for graceful cleanup, then SIGKILL every tracked descendant still alive. */
export async function stopInvocation(child: Subprocess, graceMs = 5000): Promise<Cleanup> {
  let processes = await snapshot();
  const owned = new Map<number, string>();
  // Never adopt a PID Bun has already reaped: it could belong to a new process.
  const leader = processes.get(child.pid);
  if (child.exitCode === null && child.signalCode === null && leader) {
    owned.set(child.pid, leader.birth);
  }
  // Track before signalling: a leader that exits at once orphans its descendants.
  trackDescendants(owned, processes);
  if (owned.has(child.pid)) signal(child.pid, "SIGINT");
  const deadline = performance.now() + graceMs;
  while (living(owned, processes).size > 0 && performance.now() < deadline) {
    await Bun.sleep(50);
    processes = await snapshot();
    trackDescendants(owned, processes);
  }
  // Refresh immediately before escalation, including any late descendants.
  processes = await snapshot();
  trackDescendants(owned, processes);
  // Descendants first, so the leader cannot respawn them.
  const remaining = [...living(owned, processes)].toSorted(
    (left, right) => Number(left === child.pid) - Number(right === child.pid),
  );
  for (const pid of remaining) signal(pid, "SIGKILL");
  await Promise.race([child.exited, Bun.sleep(1000)]);
  return {
    tracked_pids: [...owned.keys()].toSorted(ascending),
    escalated_pids: remaining.toSorted(ascending),
  };
}
