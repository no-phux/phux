import type { ChildProcess } from "node:child_process";

async function exitedWithin(exit: Promise<void>, milliseconds: number): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), milliseconds);
  });
  try {
    return await Promise.race([exit.then(() => true), timeout]);
  } finally {
    clearTimeout(timer);
  }
}

export async function terminateChild(child: ChildProcess, graceMs = 2_000): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null || child.pid === undefined) return;
  const exit = new Promise<void>((resolve) => child.once("exit", () => resolve()));
  child.kill("SIGTERM");
  if (await exitedWithin(exit, graceMs)) return;
  child.kill("SIGKILL");
  if (!(await exitedWithin(exit, graceMs))) {
    child.stdout?.destroy();
    child.stderr?.destroy();
    child.stdin?.destroy();
    child.unref();
    throw new Error(`Native child ${child.pid} did not terminate after SIGKILL`);
  }
}
