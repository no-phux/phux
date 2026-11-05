import { expect, test } from "bun:test";
import { parseTree, violations } from "./build-features.ts";

const CLEAN = `phux v0.102.0 (/repo/crates/phux)|default
ratatui-core v0.1.0|default,std
rustls v0.23.0|ring,std
wtransport-proto v0.6.0|
rustls v0.23.0|ring,std (*)
`;

const HOST = {
  server: parseTree("phux-server v0.120.1|\nphux-config v0.120.1|\n"),
  terminalApi: parseTree("phux-terminal-api v0.120.1|\nphux-config v0.120.1|\n"),
  config: parseTree("phux-config v0.120.1|\n"),
};

test("cargo tree rows parse to package features", () => {
  const parsed = parseTree(CLEAN);
  expect([...parsed.keys()]).toEqual(["phux", "ratatui-core", "rustls", "wtransport-proto"]);
  expect(parsed.get("rustls")).toEqual(new Set(["ring", "std"]));
});

test("clean graphs hold every boundary", () => {
  const lean = parseTree(CLEAN.replace(/^wtransport-proto.*\n/m, ""));
  expect(violations({ full: parseTree(CLEAN), lean, mcp: lean, ...HOST })).toEqual([]);
});

test("each broken boundary is reported", () => {
  const full = parseTree(
    `${CLEAN.replace("default,std", "layout-cache")}wtransport v0.6.0|\nratatui-macros v0.1.0|\n`,
  );
  const mcp = parseTree(`${CLEAN.replaceAll("ring,std", "tls12")}ratatui v0.30.0|\n`);
  expect(violations({ full, lean: parseTree(CLEAN), mcp, ...HOST })).toEqual([
    "wtransport is the test dialer; production uses wtransport-proto",
    "unused layout cache re-enabled",
    "the lean executable must not inherit server defaults",
    "headless MCP must not compile TUI chrome",
    "full: unused TUI macros",
    "mcp: TLS 1.2 re-enabled",
  ]);
});

test("each host graph rejects every native terminal dependency", () => {
  const lean = parseTree(CLEAN.replace(/^wtransport-proto.*\n/m, ""));
  for (const name of ["server", "terminalApi", "config"] as const) {
    for (const dependency of [
      "phux-terminal",
      "libghostty-vt",
      "libghostty-vt-sys",
      "portable-pty",
      "portable-pty-adopt",
    ]) {
      expect(
        violations({
          full: parseTree(CLEAN),
          lean,
          mcp: lean,
          ...HOST,
          [name]: parseTree(`${dependency} v1.0.0|\n`),
        }),
      ).toEqual([`${name}: production graph reaches ${dependency}`]);
    }
  }
});
