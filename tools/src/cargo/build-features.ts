// Production dependency feature boundaries, read from Cargo's own
// feature-unified graph (`cargo tree`) without compiling.
import { Failure } from "../lib/command.ts";
import { Expectations } from "../lib/expect.ts";
import { capture } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";

/** Package name to the features Cargo enables on it. */
type FeatureGraph = ReadonlyMap<string, ReadonlySet<string>>;

export type Graphs = Readonly<Record<"full" | "lean" | "mcp", FeatureGraph>>;

/** Parse `cargo tree --prefix none --format {p}|{f}` output. */
export function parseTree(output: string): FeatureGraph {
  return new Map(
    output
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        const [pkg = "", features = ""] = line.replace(/ \(\*\)$/, "").split("|");
        return [pkg.split(" ")[0] ?? "", new Set(features.split(","))];
      }),
  );
}

const TREE = "cargo tree --locked --edges normal,build --prefix none --format {p}|{f}".split(" ");

export function readGraphs(root = ROOT): Graphs {
  const tree = (...selection: string[]) =>
    parseTree(capture([...TREE, ...selection], { cwd: root }));
  return {
    full: tree("-p", "phux"),
    lean: tree("-p", "phux", "-p", "phux-mcp", "--no-default-features"),
    mcp: tree("-p", "phux-mcp"),
  };
}

function enables(graph: FeatureGraph, pkg: string, feature: string, label: string): boolean {
  const features = graph.get(pkg);
  if (features === undefined) throw new Failure(`${label}: ${pkg} is not in the production graph`);
  return features.has(feature);
}

/** Every violated boundary, as a message; empty when all hold. */
export function violations({ full, lean, mcp }: Graphs): string[] {
  const expect = new Expectations();
  expect.that(full.has("wtransport-proto"), "the default executable must retain browser HTTP/3");
  expect.that(
    !full.has("wtransport"),
    "wtransport is the test dialer; production uses wtransport-proto",
  );
  expect.that(
    !enables(full, "ratatui-core", "layout-cache", "full"),
    "unused layout cache re-enabled",
  );
  expect.that(
    !lean.has("wtransport-proto"),
    "the lean executable must not inherit server defaults",
  );
  expect.that(!mcp.has("ratatui"), "headless MCP must not compile TUI chrome");
  for (const [name, graph] of Object.entries({ full, lean, mcp })) {
    expect.that(!graph.has("ratatui-crossterm"), `${name}: unused terminal backend`);
    expect.that(!graph.has("ratatui-macros"), `${name}: unused TUI macros`);
    expect.that(!enables(graph, "rustls", "tls12", name), `${name}: TLS 1.2 re-enabled`);
  }
  return expect.failures;
}
