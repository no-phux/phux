import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  documentEntries,
  parseResources,
  parseSessionList,
  parseSurfaceFacts,
  parseTreeDocument,
  RESOURCE_KINDS,
  SchemaValidationError,
} from "../src/schemas.js";

test("resource kinds match the Rust ls projection's documented vocabulary", () => {
  const source = readFileSync(new URL("../../../../crates/phux-core/src/session_list.rs", import.meta.url), "utf8");
  const doc = /Resource kind: (`[^.]*`)\./s.exec(source)?.[1];
  assert.ok(doc, "ResourceJson.kind doc comment must stay discoverable");
  assert.deepEqual([...doc.matchAll(/`([a-z_]+)`/g)].map((match) => match[1]), [...RESOURCE_KINDS]);
});

test("the v4 CLI fixture parses every kind with its parent, name, producer and control", () => {
  // Captured from phux 0.129.1 against a private socket: a pane with a tree,
  // a page-controlled surface and a typed markdown document under it.
  const fixture = JSON.parse(readFileSync(new URL("../../test/fixtures/session-list-v4.json", import.meta.url), "utf8"));
  const list = parseSessionList(fixture);
  assert.equal(list.schema_version, 4);
  assert.deepEqual(list.sessions, [{ name: "fixture", windows: 1, attached: false }]);
  assert.deepEqual(list.terminals, ["@1"]);
  assert.deepEqual(list.unreachable, []);
  assert.deepEqual(list.resources.map((row) => [row.id, row.kind, row.parent, row.name]), [
    ["@1", "terminal", null, null],
    ["@2", "accessibility_tree", "@1", "Fixture"],
    ["@3", "pixel_surface", "@1", "Fixture"],
    ["@4", "document", "@1", "notes"],
  ]);
  assert.deepEqual(documentEntries(list.resources), [
    { id: "@2", kind: "accessibility_tree", parent: "@1", name: "Fixture", producer: "gone", control: null },
    { id: "@3", kind: "pixel_surface", parent: "@1", name: "Fixture", producer: "gone", control: "page" },
    { id: "@4", kind: "document", parent: "@1", name: "notes", producer: "gone", control: null },
  ]);
});

test("a mixed inventory keeps unknown kinds as opaque identity rows", () => {
  const resources = parseResources([
    { id: "@1", kind: "terminal", parent: null, lifecycle: "exited", key: null,
      exit: { status: 3, signal: null, reason: "exited", exited_at_ms: 10, retained_until_ms: 20 } },
    { id: "@2", kind: "agent_session", parent: "@1", lifecycle: "running", exit: null, key: null },
    { id: "host/@3", kind: "accessibility_tree", name: "Safari", parent: "host/@1", producer: "connected" },
    { id: "@4", kind: "hologram", parent: "@1", name: "future", depth: { nested: true }, lifecycle: 7 },
    { id: "@5", kind: "unknown", parent: null },
  ]);
  assert.deepEqual(resources[0], {
    id: "@1", kind: "terminal", name: null, parent: null, lifecycle: "exited", key: null,
    exit: { status: 3, signal: null, reason: "exited", exited_at_ms: 10, retained_until_ms: 20 },
  });
  assert.equal(resources[1]?.kind, "agent_session");
  assert.deepEqual(resources[2], {
    id: "host/@3", kind: "accessibility_tree", name: "Safari", parent: "host/@1",
    lifecycle: "running", exit: null, producer: "connected", key: null,
  });
  assert.deepEqual(resources[3], { id: "@4", kind: "hologram", name: "future", parent: "@1", opaque: true });
  assert.equal(resources[4]?.kind, "unknown");
  assert.equal("opaque" in resources[4]!, false);
  assert.deepEqual(documentEntries(resources).map((entry) => entry.id), ["host/@3"]);
});

test("resource rows reject malformed identity and known-kind fields", () => {
  const valid = { id: "@1", kind: "terminal", parent: null };
  for (const row of [
    null, [], { ...valid, id: "work" }, { ...valid, id: 1 }, { ...valid, kind: "" }, { ...valid, kind: 3 },
    { ...valid, parent: "@1 --help" }, { ...valid, name: 3 }, { ...valid, lifecycle: 1 },
    { ...valid, exit: { status: "3", exited_at_ms: 1, retained_until_ms: 2 } }, { ...valid, producer: 1 },
    { ...valid, kind: "hologram", id: "nope" },
  ]) {
    assert.throws(() => parseResources([row]), SchemaValidationError, JSON.stringify(row));
  }
  assert.throws(() => parseResources({}), /\$\.resources/);
  const list = { schema_version: 4, sessions: [], terminals: [] };
  assert.deepEqual(parseSessionList(list).resources, []);
  assert.throws(() => parseSessionList({ ...list, resources: null }), /\$\.resources/);
  assert.throws(() => parseSessionList({ ...list, unreachable: [3] }), /\$\.unreachable/);
});

test("tree and surface documents keep the CLI's facts", () => {
  assert.deepEqual(parseTreeDocument({ schema_version: 1, seq: 0, document: null }), { schema_version: 1, seq: 0, document: null });
  assert.throws(() => parseTreeDocument({ schema_version: 1, seq: 0 }), /\$\.document/);
  assert.throws(() => parseTreeDocument({ schema_version: 1, seq: -1, document: {} }), /\$\.seq/);
  assert.deepEqual(parseSurfaceFacts({
    schema_version: 1, seq: 1, bytes: 70, format: "png", width: 1, height: 1, content_type: null, out: "/tmp/frame",
  }), { schema_version: 1, seq: 1, bytes: 70, format: "png", width: 1, height: 1, content_type: null, out: "/tmp/frame" });
  assert.equal(parseSurfaceFacts({ schema_version: 1, seq: 0, bytes: 0, format: null, width: null, height: null }).out, null);
  assert.throws(() => parseSurfaceFacts({ schema_version: 1, seq: 1, bytes: 2, format: "gif", width: 1, height: 1 }), /\$\.format/);
});
