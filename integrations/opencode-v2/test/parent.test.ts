import { describe, expect, test } from "bun:test";

import { isWriteTool, parentPane, parentWriteError, samePane } from "../src/parent.js";
import { createGuardedTools } from "../src/tools.js";
import type { PhuxToolRuntime } from "../src/tools-core.js";

describe("parent pane", () => {
  test("normalizes a bare terminal id", () => {
    expect(parentPane("7")).toBe("@7");
    expect(parentPane("@7")).toBe("@7");
    expect(parentPane(undefined)).toBeNull();
    expect(parentPane("  ")).toBeNull();
  });

  test("matches only the same local pane", () => {
    expect(samePane("7", "@7")).toBe(true);
    expect(samePane("@7", "7")).toBe(true);
    expect(samePane("@8", "@7")).toBe(false);
    expect(samePane("mac/@7", "@7")).toBe(false);
  });

  test("write tools are the ones that type", () => {
    expect(isWriteTool("phux_run")).toBe(true);
    expect(isWriteTool("phux_send_keys")).toBe(true);
    expect(isWriteTool("phux_snapshot")).toBe(false);
    expect(isWriteTool("phux_wait")).toBe(false);
    expect(isWriteTool("phux_create")).toBe(false);
  });
});

describe("guarded tools", () => {
  test("refuses a write into the parent before calling phux", async () => {
    let called = false;
    const runtime = fakeRuntime(() => {
      called = true;
    });
    const tools = createGuardedTools(runtime, "@7");
    await expect(tools.phux_run!.execute({ command: "ls", target: "7" }, context())).rejects.toThrow(/Refusing phux_run on @7/);
    expect(called).toBe(false);
    expect(parentWriteError("phux_run", "@7").message).toContain("sibling");
  });

  test("allows a write to a sibling and a read of the parent", async () => {
    const seen: string[] = [];
    const runtime = fakeRuntime((name) => {
      seen.push(name);
    });
    const tools = createGuardedTools(runtime, "@7");
    await tools.phux_run!.execute({ command: "ls", target: "@8" }, context());
    await tools.phux_snapshot!.execute({ target: "@7" }, context());
    expect(seen).toEqual(["run", "snapshot"]);
  });

  test("refuses a write that falls through to the parent", async () => {
    let called = false;
    const runtime = fakeRuntime(() => {
      called = true;
    });
    runtime.environmentTarget = "@7";
    const tools = createGuardedTools(runtime, "@7");
    await expect(tools.phux_send_keys!.execute({ keys: ["Enter"] }, context())).rejects.toThrow(/Refusing phux_send_keys/);
    expect(called).toBe(false);
  });

  test("does not guard when OpenCode is not inside a pane", async () => {
    const seen: string[] = [];
    const tools = createGuardedTools(fakeRuntime((name) => seen.push(name)), null);
    await tools.phux_send_keys!.execute({ keys: ["Enter"], target: "@7" }, context());
    expect(seen).toEqual(["sendKeys"]);
    expect(tools.phux_run!.description).not.toContain("Refuses the pane");
  });
});

function context() {
  return { sessionID: "ses", agent: "build", messageID: "msg", id: "call" };
}

function fakeRuntime(onCall: (name: string) => void): PhuxToolRuntime {
  const cli = {
    ls: async () => ({ sessions: [] }),
    create: async () => {
      throw new Error("unused");
    },
    snapshot: async () => {
      onCall("snapshot");
      return { pane: 7, cols: 80, rows: 24, lines: [], scrollback: [], cursor: { x: 0, y: 0, visible: true } };
    },
    sendKeys: async () => {
      onCall("sendKeys");
    },
    run: async () => {
      onCall("run");
      return { exit_code: 0, output: "", duration_ms: 1, truncated: false, command: "ls" };
    },
    wait: async () => {
      throw new Error("unused");
    },
  };
  return {
    cli: cli as unknown as PhuxToolRuntime["cli"],
    getSelectedTarget: () => undefined,
    selectTarget: () => undefined,
  };
}
