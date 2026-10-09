import { describe, expect, test } from "bun:test";

import { redirectShell } from "../src/shell.js";

describe("OpenCode shell redirect", () => {
  test("runs the selected sibling through phux and refuses the parent pane", () => {
    const run = redirectShell({
      command: "pwd",
      parent: "@7",
      siblings: ["@8"],
      executable: "phux",
      socket: "/tmp/phux.sock",
      timeoutMs: 30_000,
    });
    expect(run).toEqual({
      kind: "run",
      command: "'phux' run --json --timeout 30 --socket '/tmp/phux.sock' '@8' 'pwd'",
    });

    const refused = redirectShell({
      command: "pwd",
      parent: "@7",
      siblings: ["@7"],
      executable: "phux",
      timeoutMs: 30_000,
    });
    expect(refused.kind).toBe("refuse");
  });

  test("leaves the built-in shell alone when OpenCode is not inside phux", () => {
    expect(
      redirectShell({
        command: "pwd",
        parent: null,
        siblings: [],
        executable: "phux",
        timeoutMs: 30_000,
      }),
    ).toEqual({ kind: "passthrough" });
  });

  test("refuses when more than one sibling is selected", () => {
    const decision = redirectShell({
      command: "pwd",
      parent: "@7",
      siblings: ["@8", "@9"],
      executable: "phux",
      timeoutMs: 30_000,
    });
    expect(decision.kind).toBe("refuse");
  });
});
