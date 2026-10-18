// Bounded shutdown for an invocation whose descendants may leave its process
// group: cargo-mutants starts Cargo and tests in groups of their own.
import type { Subprocess } from "bun";

/** pid -> (parent pid, start time). */
export type Snapshot = Map<number, { parent: number; birth: string }>;

/** Portable PID/PPID traversal; birth times prevent targeting reused PIDs. */
export function processSnapshot(): Snapshot {
  const result = Bun.spawnSync(["ps", "-axo", "pid=,ppid=,lstart="], {
    stdout: "pipe",
    stderr: "pipe",
    timeout: 1000,
  });
  if (result.exitCode !== 0) throw new Error(`ps exited ${result.exitCode}`);
  return parseSnapshot(result.stdout.toString());
}

export function parseSnapshot(text: string): Snapshot {
  const processes: Snapshot = new Map();
  for (const line of text.split("\n")) {
    const match = /^\s*(\d+)\s+(\d+)\s+(.+?)\s*$/.exec(line);
    if (match?.[1] && match[2] && match[3]) {
      processes.set(Number(match[1]), { parent: Number(match[2]), birth: match[3] });
    }
  }
  return processes;
}

/** Owned PIDs still running as the process they were when first seen. */
export function living(owned: Map<number, string>, processes: Snapshot): Set<number> {
  const alive = new Set<number>();
  for (const [pid, birth] of owned) {
    if (processes.get(pid)?.birth === birth) alive.add(pid);
  }
  return alive;
}

/** Retain identities even after a known child is orphaned/reparented. */
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

function signalProcess(pid: number, signal: NodeJS.Signals): void {
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (!(error instanceof Error && "code" in error && error.code === "ESRCH")) throw error;
  }
}

export interface Cleanup {
  tracked_pids: number[];
  escalated_pids: number[];
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Allow cargo-mutants cleanup, then escalate across its owned child groups. */
export async function stopInvocation(child: Subprocess, graceMs = 5000): Promise<Cleanup> {
  let processes = processSnapshot();
  const owned = new Map<number, string>();
  // An unexited direct child retains its PID. Never adopt a PID after Bun
  // has reaped it: it could belong to a new process.
  const leader = processes.get(child.pid);
  if (child.exitCode === null && child.signalCode === null && leader) {
    owned.set(child.pid, leader.birth);
  }
  trackDescendants(owned, processes);
  if (living(owned, processes).has(child.pid)) signalProcess(child.pid, "SIGINT");
  const deadline = performance.now() + graceMs;
  while (living(owned, processes).size > 0 && performance.now() < deadline) {
    await sleep(50);
    processes = processSnapshot();
    trackDescendants(owned, processes);
  }
  // Refresh immediately before escalation, including any late descendants.
  processes = processSnapshot();
  trackDescendants(owned, processes);
  const remaining = [...living(owned, processes)];
  remaining.sort((left, right) => Number(left === child.pid) - Number(right === child.pid));
  for (const pid of remaining) signalProcess(pid, "SIGKILL");
  await Promise.race([child.exited, sleep(1000)]);
  return {
    tracked_pids: [...owned.keys()].sort((a, b) => a - b),
    escalated_pids: remaining.toSorted((a, b) => a - b),
  };
}
