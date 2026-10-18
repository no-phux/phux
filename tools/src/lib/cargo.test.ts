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
