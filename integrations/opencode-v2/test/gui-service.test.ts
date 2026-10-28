import { expect, test } from "bun:test";

import { PhuxError } from "../../runtime/src/errors.js";
import { createGuiService, registerTerminals } from "../src/gui-service.js";
import { OwnedTerminals } from "../src/terminals.js";

function agent(terminal: string, state: string) {
  return {
    terminal,
    session: "work",
    window: "window-0",
    agent: { id: "claude", label: "Claude", kind: "claude" },
    agent_session: { resource: terminal, provider: "claude", native_id: "native" },
    state,
    confidence: 1,
    attention: "normal",
    title: "Refactor",
    cwd: "/repo",
    sources: [],
    explanation: "",
  };
}

const gone = () => new PhuxError("command_failed", "no such target: @9", {
  exitCode: 1,
  cliError: { error: { code: "no_such_target", message: "no such target: @9" } },
});

function fakeCli(socket: string) {
  const calls: string[] = [];
  const state = { agentState: "working", html: "<div>one</div>", renderedFails: undefined as Error | undefined, spawned: 20 };
  const cli = {
    executable: "phux",
    socket,
    calls,
    state,
    async agentList() {
      calls.push("agentList");
      return { schema_version: 1 as const, agents: [agent("@3", state.agentState)] };
    },
    async watchUntil(target: string, options: { after?: string; until: readonly string[]; timeoutSeconds: number }) {
      calls.push(`watch ${target} after=${options.after ?? "-"} until=${options.until.join(",")}`);
      if (target === "@9") throw gone();
      return { outcome: "matched" as const, event: "dirty", cursor: "c2" };
    },
    async rendered(target: string) {
      calls.push(`rendered ${target}`);
      if (state.renderedFails !== undefined) throw state.renderedFails;
      return { pane: 3, cols: 80, rows: 2, cursor: { x: 0, y: 1, visible: true }, title: "t", html: state.html };
    },
    async snapshot(options: { target?: string }) {
      calls.push(`snapshot ${options.target}`);
      return { schema_version: 3 as const, pane: 3, cols: 80, rows: 2, cursor: { x: 0, y: 1, visible: true }, lines: ["a", "b"], scrollback: ["h"] };
    },
    async paste(target: string, text: string) {
      calls.push(`paste ${target} ${text}`);
    },
    async sendKeys(target: string, keys: readonly string[]) {
      calls.push(`keys ${target} ${keys.join(" ")}`);
    },
    async agentPrompt(target: string, text: string) {
      calls.push(`prompt ${target} ${text}`);
      return {};
    },
    async spawn(options: { cwd?: string }) {
      calls.push(`spawn ${options.cwd ?? "-"}`);
      return { terminal_id: state.spawned++, satellite: null };
    },
    async ls() {
      return { schema_version: 3 as const, sessions: [], terminals: [] };
    },
    async kill(target: string) {
      calls.push(`kill ${target}`);
    },
    async tag() {
      return [];
    },
  };
  return cli;
}

function service(socket: string, parent: string | null = null) {
  const cli = fakeCli(socket);
  const terminals = new OwnedTerminals(cli as never, 2);
  const unregister = registerTerminals(terminals);
  const selected = new Map<string, string>();
  const gui = createGuiService({ cli: cli as never, screen: cli as never, terminals, maxTerminals: 2, parent, selectedTargets: selected });
  return { cli, gui, selected, unregister };
}

test("overview is one CLI call per second for every window, and numbers each state change once", async () => {
  const { cli, gui, unregister } = service("/tmp/overview.sock");
  try {
    const first = await gui.overview({ sessionIDs: ["s"] });
    await gui.overview({});
    expect(cli.calls.filter((call) => call === "agentList")).toHaveLength(1);
    expect(first.available).toBe(true);
    expect(first.owned).toEqual({ s: [] });
    expect(first.panes[0]).toMatchObject({ target: "@3", state: "working", seq: 0, provider: "claude", nativeId: "native" });

    await Bun.sleep(1050);
    cli.state.agentState = "blocked";
    const second = await gui.overview({});
    expect(second.panes[0]).toMatchObject({ state: "blocked", seq: 1 });
  } finally {
    unregister();
  }
});

test("an unreachable phux is a state the GUI shows, not a failed call", async () => {
  const { cli, gui, unregister } = service("/tmp/down.sock");
  cli.agentList = async () => {
    throw new PhuxError("unavailable", "phux executable \"phux\" was not found");
  };
  try {
    expect(await gui.overview({})).toMatchObject({ available: false, error: expect.stringContaining("not found"), panes: [] });
  } finally {
    unregister();
  }
});

test("a screen long-polls after the cursor and skips a render the window already has", async () => {
  const { cli, gui, unregister } = service("/tmp/screen.sock");
  try {
    const first = await gui.screen({ target: "@3", waitSeconds: 0 });
    expect(first).toMatchObject({ changed: true, closed: false, html: "<div>one</div>", historyRows: 1, rows: 2, caret: { x: 0, y: 1 } });
    expect(cli.calls.some((call) => call.startsWith("watch"))).toBe(false);

    const same = await gui.screen({ target: "@3", waitSeconds: 5, after: "c1", known: first.hash! });
    expect(same).toEqual({ changed: false, closed: false, cursor: "c2", event: "dirty", hash: first.hash });
    expect(cli.calls).toContain("watch @3 after=c1 until=dirty,idle,title_changed,pane_closed,unknown");

    cli.state.html = "<div>two</div>";
    const next = await gui.screen({ target: "@3", waitSeconds: 5, after: "c2", known: first.hash! });
    expect(next).toMatchObject({ changed: true, html: "<div>two</div>", cursor: "c2" });
    expect(next.hash).not.toBe(first.hash);
  } finally {
    unregister();
  }
});

test("a closed pane reads as closed, and an older phux falls back to plain rows", async () => {
  const { cli, gui, unregister } = service("/tmp/closed.sock");
  try {
    expect(await gui.screen({ target: "@9", waitSeconds: 5 })).toEqual({ changed: true, closed: true });
    // A missing executable is an error to show, not a closed pane.
    const missing = cli.rendered;
    cli.rendered = async () => {
      throw new PhuxError("unavailable", "phux executable \"phux\" was not found");
    };
    await expect(gui.screen({ target: "@3" })).rejects.toThrow("was not found");
    cli.rendered = missing;
    // A phux that cannot resume watches still renders after a short poll.
    const watch = cli.watchUntil;
    cli.watchUntil = async () => {
      throw new PhuxError("command_failed", "unexpected argument '--after'", { exitCode: 2 });
    };
    expect(await gui.screen({ target: "@3", waitSeconds: 1 })).toMatchObject({ changed: true, closed: false });
    cli.watchUntil = watch;
    cli.state.renderedFails = new PhuxError("command_failed", "unexpected argument '--format'", { exitCode: 2 });
    expect(await gui.screen({ target: "@3" })).toMatchObject({ changed: true, lines: ["h", "a", "b"], historyRows: 1 });
    cli.state.renderedFails = new PhuxError("timeout", "slow");
    await expect(gui.screen({ target: "@3" })).rejects.toThrow("slow");
  } finally {
    unregister();
  }
});

test("input reaches only exact panes, never the pane hosting this server", async () => {
  const { cli, gui, unregister } = service("/tmp/input.sock", "@7");
  try {
    await gui.input({ target: "@3", text: "ls -la", submit: true });
    await gui.input({ target: "@3", keys: ["C-c"] });
    expect(cli.calls.slice(-3)).toEqual(["paste @3 ls -la", "keys @3 Enter", "keys @3 C-c"]);
    await expect(gui.input({ target: "@7", keys: ["C-c"] })).rejects.toThrow("hosts this OpenCode server");
    await expect(gui.input({ target: "@007", keys: ["C-c"] })).rejects.toThrow("hosts this OpenCode server");
    await expect(gui.prompt({ target: "@07", text: "hi" })).rejects.toThrow("hosts this OpenCode server");
    await gui.input({ target: "@03", keys: ["-h", "--", "--socket=/x", "-", "Enter"] });
    expect(cli.calls.at(-1)).toBe("keys @3 - h - - - - s o c k e t = / x - Enter");
    await expect(gui.input({ target: "#all", keys: ["C-c"] })).rejects.toThrow("exact");
    await expect(gui.screen({ target: "%reviewer" })).rejects.toThrow("exact");
    await expect(gui.input({ target: "@3" })).rejects.toThrow("needs text");
  } finally {
    unregister();
  }
});

test("a single-line prompt gets phux's receipt, a multi-line one is pasted and submitted", async () => {
  const { cli, gui, unregister } = service("/tmp/prompt.sock");
  try {
    await gui.prompt({ target: "@3", text: "fix the test" });
    await gui.prompt({ target: "@3", text: "line one\nline two" });
    expect(cli.calls.slice(-3)).toEqual(["prompt @3 fix the test", "paste @3 line one\nline two", "keys @3 Enter"]);
  } finally {
    unregister();
  }
});

test("terminals spawned from the GUI belong to the session, count against its cap, and close only by it", async () => {
  const a = service("/tmp/own-a.sock");
  const b = service("/tmp/own-b.sock");
  try {
    const { target } = await a.gui.spawn({ sessionID: "s", cwd: "/repo" });
    expect(target).toBe("@20");
    expect(a.selected.get("s")).toBe("@20");
    // Another location's instance sees the session's terminals and can close them.
    expect((await b.gui.overview({ sessionIDs: ["s", "t"] })).owned).toEqual({ s: ["@20"], t: [] });
    await expect(b.gui.kill({ sessionID: "t", target: "@20" })).rejects.toThrow("was not spawned by this session");
    await b.gui.kill({ sessionID: "s", target: "@20" });
    expect(a.cli.calls).toContain("kill @20");
    expect((await a.gui.overview({ sessionIDs: ["s"] })).owned).toEqual({ s: [] });

    await a.gui.spawn({ sessionID: "s" });
    await a.gui.spawn({ sessionID: "s" });
    await expect(a.gui.spawn({ sessionID: "s" })).rejects.toThrow("already owns 2");
  } finally {
    a.unregister();
    b.unregister();
  }
});
