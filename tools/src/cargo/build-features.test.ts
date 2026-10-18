import { describe, expect, test } from "bun:test";
import { parseTree, violations, type FeatureGraph } from "./build-features.ts";

const CLEAN = `phux v0.102.0 (/repo/crates/phux)|default
ratatui-core v0.1.0|default,std
rustls v0.23.0|ring,std
wtransport-proto v0.6.0|
rustls v0.23.0|ring,std (*)
`;

function graph(text: string): FeatureGraph {
  return parseTree(text);
}

describe("build features", () => {
  test("cargo tree rows parse to package features", () => {
    const parsed = graph(CLEAN);
    expect([...parsed.keys()]).toEqual(["phux", "ratatui-core", "rustls", "wtransport-proto"]);
    expect([...(parsed.get("rustls") ?? [])]).toEqual(["ring", "std"]);
    expect([...(parsed.get("wtransport-proto") ?? [])]).toEqual([""]);
  });

  test("clean graphs hold every boundary", () => {
    const lean = graph(CLEAN.replace(/^wtransport-proto.*\n/m, ""));
    expect(violations({ full: graph(CLEAN), lean, mcp: lean })).toEqual([]);
  });

  test("each broken boundary is reported", () => {
    const full = graph(
      `${CLEAN.replace("default,std", "layout-cache")}wtransport v0.6.0|\nratatui-macros v0.1.0|\n`,
    );
    const mcp = graph(`${CLEAN.replaceAll("ring,std", "tls12")}ratatui v0.30.0|\n`);
    expect(violations({ full, lean: graph(CLEAN), mcp })).toEqual([
      "wtransport is the test dialer; production uses wtransport-proto",
      "unused layout cache re-enabled",
      "the lean executable must not inherit server defaults",
      "headless MCP must not compile TUI chrome",
      "full: unused TUI macros",
      "mcp: TLS 1.2 re-enabled",
    ]);
  });
});
