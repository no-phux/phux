import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathClosure, workspaceMembers } from "./cargo.ts";

const root = mkdtempSync(join(tmpdir(), "phux-cargo-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function write(path: string, text: string): void {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), text);
}

write(
  "Cargo.toml",
  `[workspace]
members = ["crates/*"]
exclude = ["clients"]
[workspace.dependencies]
core = { path = "crates/core" }
renamed-wire = { path = "crates/wire", package = "wire" }
serde = "1"
`,
);
write(
  "crates/core/Cargo.toml",
  `[package]\nname = "core"\n[dependencies]\nserde.workspace = true\n`,
);
write(
  "crates/wire/Cargo.toml",
  `[package]\nname = "wire"\n[dependencies]\ncore = { workspace = true }\n`,
);
write("crates/testkit/Cargo.toml", `[package]\nname = "testkit"\n`);
write(
  "crates/server/Cargo.toml",
  `[package]
name = "server"
[dependencies]
renamed-wire.workspace = true
[dev-dependencies]
testkit = { path = "../testkit" }
[target.'cfg(unix)'.build-dependencies]
unixy = { path = "../unixy" }
`,
);
write("crates/unixy/Cargo.toml", `[package]\nname = "unixy"\n`);
write(
  "clients/app/Cargo.toml",
  `[package]
name = "app"
[workspace]
[dependencies]
server = { path = "../../crates/server" }
[dev-dependencies]
wire = { path = "../../crates/wire" }
`,
);

// A separate workspace exercises feature activation without changing real manifests.
write(
  "features/Cargo.toml",
  `[workspace]
[workspace.dependencies]
renamed = { path = "inherited", package = "inherited", default-features = false, features = ["base"] }
`,
);
const featureCrates: Record<string, string> = {
  app: `[features]
default = ["b-on", "weak"]
b-on = ["dep:b"]
weak = ["e?/x"]
strong = ["c/x"]
cycle = ["cycle-again"]
cycle-again = ["cycle", "dep:c"]
[dependencies]
b = { path = "../b", optional = true }
c = { path = "../c", optional = true, default-features = false }
e = { path = "../e", optional = true }
renamed = { workspace = true, features = ["extra"] }
`,
  b: `[dependencies]
f = { path = "../f", features = ["g"] }
`,
  c: `[features]
default = ["dep:unused"]
x = ["dep:g"]
[dependencies]
g = { path = "../g", optional = true }
unused = { path = "../unused", optional = true }
`,
  e: `[features]
x = ["dep:g"]
[dependencies]
g = { path = "../g", optional = true }
`,
  f: `[features]
g = ["dep:g"]
[dependencies]
g = { path = "../g", optional = true }
`,
  inherited: `[features]
default = ["dep:unused"]
base = ["dep:base-leaf"]
extra = ["dep:extra-leaf"]
[dependencies]
unused = { path = "../unused", optional = true }
base-leaf = { path = "../base-leaf", optional = true }
extra-leaf = { path = "../extra-leaf", optional = true }
`,
  g: "",
  unused: "",
  "base-leaf": "",
  "extra-leaf": "",
};
for (const [name, body] of Object.entries(featureCrates)) {
  write(`features/${name}/Cargo.toml`, `[package]\nname = "${name}"\n${body}`);
}

function featureClosure(features: string[] = [], defaultFeatures = true): string[] {
  return [...pathClosure([{ dir: "features/app", dev: false, features, defaultFeatures }], root)]
    .map((dir) => dir.replace("features/", ""))
    .sort();
}

test("optional dependencies follow default aliases, dep: and forwarded edge features", () => {
  expect(featureClosure()).toEqual(["app", "b", "base-leaf", "extra-leaf", "f", "g", "inherited"]);
});

test("default-features false, workspace features and explicit root features are honored", () => {
  expect(featureClosure([], false)).toEqual(["app", "base-leaf", "extra-leaf", "inherited"]);
  expect(featureClosure(["strong"], false)).toEqual([
    "app",
    "base-leaf",
    "c",
    "extra-leaf",
    "g",
    "inherited",
  ]);
});

test("weak forwarding does not activate an optional dependency, but applies once enabled", () => {
  expect(featureClosure(["weak"], false)).not.toContain("e");
  expect(featureClosure(["weak", "e"], false)).toEqual([
    "app",
    "base-leaf",
    "e",
    "extra-leaf",
    "g",
    "inherited",
  ]);
});

test("feature aliases can cycle, and implicit optional features activate dependencies", () => {
  expect(featureClosure(["cycle"], false)).toEqual([
    "app",
    "base-leaf",
    "c",
    "extra-leaf",
    "inherited",
  ]);
});

test("later feature requests revisit dependencies to a fixed point, regardless of root order", () => {
  const builds = [
    { dir: "features/c", dev: false, defaultFeatures: false },
    { dir: "features/app", dev: false, defaultFeatures: false, features: ["strong"] },
  ];
  for (const roots of [builds, builds.toReversed()]) {
    expect([...pathClosure(roots, root)].sort()).toEqual([
      "features/app",
      "features/base-leaf",
      "features/c",
      "features/extra-leaf",
      "features/g",
      "features/inherited",
    ]);
  }
});

test("members come from the workspace globs", () => {
  expect(workspaceMembers(".", root).map(({ dir, name }) => `${dir}=${name}`)).toEqual([
    "crates/core=core",
    "crates/server=server",
    "crates/testkit=testkit",
    "crates/unixy=unixy",
    "crates/wire=wire",
  ]);
});

test("a closure follows path, inherited, renamed and target-specific edges", () => {
  expect([...pathClosure([{ dir: "crates/server", dev: false }], root)].sort()).toEqual([
    "crates/core",
    "crates/server",
    "crates/unixy",
    "crates/wire",
  ]);
});

test("dev dependencies count only at a dev root", () => {
  expect([...pathClosure([{ dir: "crates/server", dev: true }], root)].sort()).toContain(
    "crates/testkit",
  );
  // app builds server as a dependency, so server's dev dependencies stay out;
  // app's own dev dependency on wire counts.
  expect([...pathClosure([{ dir: "clients/app", dev: true }], root)].sort()).toEqual([
    "clients/app",
    "crates/core",
    "crates/server",
    "crates/unixy",
    "crates/wire",
  ]);
});
