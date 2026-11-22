import assert from "node:assert/strict";
import test from "node:test";

import type { AgentPane } from "../src/schemas.js";
import {
  PHUX_NAMED_TARGETS_ENTRY,
  PHUX_TARGET_ENTRY,
  PhuxTargetStore,
  formatTargetStatus,
  type PhuxTargetSelection,
} from "../src/target-store.js";

const pane: AgentPane = {
  terminal: "@3",
  session: "work",
  window: "window-0",
  agent: { id: "codex", label: "Codex", kind: "codex" },
  state: "working",
  confidence: 0.9,
  attention: "normal",
  title: "do not persist this title",
  cwd: "/repo",
  sources: [],
  explanation: "working cue",
};

const saved: PhuxTargetSelection = {
  version: 1,
  selector: "@3",
  session: "work",
  window: "window-0",
  display: "work:window-0 @3 - Codex",
};

test("selection persists a versioned canonical target with ownership fields", () => {
  const entries: Array<{ customType: string; data: unknown }> = [];
  const store = new PhuxTargetStore(
    { appendEntry: (customType, data) => entries.push({ customType, data }) },
    { agentList: async () => ({ agents: [pane] }) },
  );

  const selection = store.select(pane);

  assert.deepEqual(selection, saved);
  assert.deepEqual(entries, [{ customType: PHUX_TARGET_ENTRY, data: saved }]);
  assert.equal(store.snapshot.availability, "available");
});

test("created seed panes persist enough ownership for branch reconstruction", () => {
  const entries: Array<{ customType: string; data: unknown }> = [];
  const store = new PhuxTargetStore(
    { appendEntry: (customType, data) => entries.push({ customType, data }) },
    { agentList: async () => ({ agents: [] }) },
  );

  const selection = store.selectCreated("fresh", 12);

  assert.deepEqual(selection, {
    version: 1,
    selector: "@12",
    session: "fresh",
    window: "window-0",
    display: "fresh:window-0 @12",
  });
  assert.deepEqual(entries, [{ customType: PHUX_TARGET_ENTRY, data: selection }]);
});

test("restore uses only the latest selection on the supplied branch", async () => {
  const otherBranch = { ...saved, selector: "@1", display: "other @1" };
  const store = new PhuxTargetStore(
    { appendEntry: () => {} },
    { agentList: async () => ({ agents: [pane] }) },
  );

  store.restoreFromBranch([
    { type: "custom", customType: PHUX_TARGET_ENTRY, data: otherBranch },
    { type: "message" },
    { type: "custom", customType: PHUX_TARGET_ENTRY, data: saved },
  ]);
  await store.refresh();

  assert.deepEqual(store.snapshot.selection, saved);
  assert.equal(store.snapshot.availability, "available");
});

test("missing restored panes stay selected and are reported stale without fallback", async () => {
  const store = new PhuxTargetStore(
    { appendEntry: () => {} },
    { agentList: async () => ({ agents: [{ ...pane, terminal: "@9" }] }) },
  );
  store.restoreFromBranch([{ type: "custom", customType: PHUX_TARGET_ENTRY, data: saved }]);

  await store.refresh();

  assert.equal(store.snapshot.selection?.selector, "@3");
  assert.equal(store.snapshot.availability, "stale");
  assert.match(formatTargetStatus(store.snapshot), /stale/);
});

test("reused pane ids with different ownership are stale", async () => {
  const store = new PhuxTargetStore(
    { appendEntry: () => {} },
    { agentList: async () => ({ agents: [{ ...pane, session: "other", window: "window-9" }] }) },
  );
  store.restoreFromBranch([{ type: "custom", customType: PHUX_TARGET_ENTRY, data: saved }]);

  await store.refresh();

  assert.equal(store.snapshot.selection?.selector, "@3");
  assert.equal(store.snapshot.availability, "stale");
  assert.equal(
    store.snapshot.reason,
    "pane @3 now belongs to other:window-9; expected work:window-0",
  );
});

test("listeners receive only ownership-validated available targets", async () => {
  let inventory: readonly AgentPane[] = [pane];
  const store = new PhuxTargetStore(
    { appendEntry: () => {} },
    { agentList: async () => ({ agents: inventory }) },
  );
  const published: Array<PhuxTargetSelection | null> = [];
  store.subscribe((selection) => published.push(selection));

  store.restoreFromBranch([{ type: "custom", customType: PHUX_TARGET_ENTRY, data: saved }]);
  assert.deepEqual(published, []);
  await store.refresh();
  assert.deepEqual(published, [saved]);

  inventory = [{ ...pane, session: "foreign" }];
  await store.refresh();
  assert.deepEqual(published, [saved, null]);
  assert.equal(store.snapshot.availability, "stale");
});

test("named aliases and groups persist as branch-local canonical ownership snapshots", async () => {
  const entries: Array<{ customType: string; data: unknown }> = [];
  const store = new PhuxTargetStore(
    { appendEntry: (customType, data) => entries.push({ customType, data }) },
    { agentList: async () => ({ agents: [pane, { ...pane, terminal: "@4", window: "window-1" }] }) },
  );
  await store.refresh();
  const first = store.selectionFor("@3");
  const second = store.selectionFor("@4");
  assert.notEqual(first, null);
  assert.notEqual(second, null);

  store.bindAlias("build", first as PhuxTargetSelection);
  store.bindGroup("workers", [first, second] as PhuxTargetSelection[]);

  assert.deepEqual(store.resolveAlias("build"), first);
  assert.deepEqual(store.resolveGroup("workers"), [first, second]);
  assert.equal(entries[0]?.customType, PHUX_NAMED_TARGETS_ENTRY);
  assert.equal(entries[1]?.customType, PHUX_NAMED_TARGETS_ENTRY);

  const restored = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: [pane, { ...pane, terminal: "@4", window: "window-1" }] }) });
  restored.restoreFromBranch(entries.map((entry) => ({ type: "custom", ...entry })));
  await restored.refresh();
  assert.equal(restored.resolveAlias("build").selector, "@3");
  assert.deepEqual(restored.resolveGroup("workers").map((selection) => selection.selector), ["@3", "@4"]);
});

test("named targets reject stale pane-id ownership and invalid names", async () => {
  let panes: readonly AgentPane[] = [pane];
  const store = new PhuxTargetStore({ appendEntry: () => {} }, { agentList: async () => ({ agents: panes }) });
  await store.refresh();
  const selection = store.selectionFor("@3") as PhuxTargetSelection;
  assert.throws(() => store.bindAlias("bad name", selection), /target names/);
  store.bindAlias("build", selection);
  panes = [{ ...pane, session: "foreign" }];
  await store.refresh();
  assert.throws(() => store.resolveAlias("build"), /ownership changed/);
});

test("inventory failures preserve the target and expose unavailable state", async () => {
  const store = new PhuxTargetStore(
    { appendEntry: () => {} },
    { agentList: async () => { throw new Error("server offline"); } },
  );
  store.restoreFromBranch([{ type: "custom", customType: PHUX_TARGET_ENTRY, data: saved }]);

  await store.refresh();

  assert.equal(store.snapshot.selection?.selector, "@3");
  assert.equal(store.snapshot.availability, "unavailable");
  assert.equal(store.snapshot.reason, "server offline");
});
