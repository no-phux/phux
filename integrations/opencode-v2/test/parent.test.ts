import { describe, expect, test } from "bun:test";

import { parentPane } from "../src/parent.js";
import { createPhuxTools, type PhuxToolRuntime } from "../../runtime/src/tools.js";

describe("parent pane", () => {
  test("normalizes the hosting terminal identity", () => {
    expect(parentPane("7")).toBe("@7");
    expect(parentPane("@7")).toBe("@7");
    expect(parentPane(undefined)).toBeNull();
    expect(parentPane("  ")).toBeNull();
  });

  test("refuses every parent input path before calling the CLI", async () => {
    const calls: string[] = [];
    const tools = createPhuxTools(fakeRuntime(calls, "@7"));
    const writes = [
      ["phux_run", { command: "pwd", target: "@7" }],
      ["phux_send_keys", { keys: ["Enter"] }],
      ["phux_paste", { text: "hello", target: "@7" }],
      ["phux_agent_prompt", { text: "hello", target: "@7" }],
    ] as const;
    for (const [name, input] of writes) {
      await expect(tools[name]!.execute(input, context)).rejects.toThrow();
    }
    expect(calls).toEqual([]);
  });

  test("allows sibling input and parent observation without confusing remote IDs", async () => {
    const calls: string[] = [];
    const tools = createPhuxTools(fakeRuntime(calls));
    await tools.phux_run!.execute({ command: "pwd", target: "@8" }, context);
    await tools.phux_run!.execute({ command: "pwd", target: "mac/@7" }, context);
    await tools.phux_snapshot!.execute({ target: "@7" }, context);
    expect(calls).toEqual(["run:@8", "run:mac/@7", "snapshot:@7"]);
  });
});

const context = { sessionID: "ses", agent: "build", messageID: "msg", id: "call" };

function fakeRuntime(calls: string[], environmentTarget?: string): PhuxToolRuntime {
  const cli = {
    snapshot: async (options: { target: string }) => {
      calls.push(`snapshot:${options.target}`);
      return { pane: 7, cols: 80, rows: 24, lines: [], scrollback: [], cursor: { x: 0, y: 0, visible: true } };
    },
    run: async (target: string) => {
      calls.push(`run:${target}`);
      return { exit_code: 0, output: "", duration_ms: 1, truncated: false, command: "pwd" };
    },
    sendKeys: async () => { calls.push("sendKeys"); },
    paste: async () => { calls.push("paste"); },
    agentPrompt: async () => { calls.push("agentPrompt"); },
  };
  return {
    cli: cli as unknown as PhuxToolRuntime["cli"],
    parentTarget: "@7",
    ...(environmentTarget === undefined ? {} : { environmentTarget }),
    getSelectedTarget: () => undefined,
    selectTarget: () => undefined,
  };
}
