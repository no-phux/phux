import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { terminateChild } from "../../scripts/terminate-child";

test("cleanup escalates and reaps a child that ignores SIGTERM", async () => {
  const child = spawn(
    process.execPath,
    ["-e", 'process.on("SIGTERM", () => {}); console.log("ready"); setInterval(() => {}, 1000);'],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  try {
    await new Promise<void>((resolve, reject) => {
      child.once("error", reject);
      child.stdout.once("data", () => resolve());
    });
    await terminateChild(child, 100);
    expect(child.signalCode).toBe("SIGKILL");
  } finally {
    child.kill("SIGKILL");
  }
});
