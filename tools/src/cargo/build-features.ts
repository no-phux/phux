// Check production dependency boundaries without compiling or resolving dev
// deps: Cargo's own feature-unified graph is the evidence, so nothing here
// restates which crate enables what.
import { Expectations } from "../lib/expect.ts";
import { ROOT } from "../lib/repo.ts";

/** Package name to the features Cargo enables on it. */
export type FeatureGraph = ReadonlyMap<string, ReadonlySet<string>>;

/** Parse `cargo tree --prefix none --format {p}|{f}` output. */
export function parseTree(output: string): FeatureGraph {
  const graph = new Map<string, Set<string>>();
  for (const line of output.split("\n")) {
    if (line === "") continue;
    const row = line.endsWith(" (*)") ? line.slice(0, -" (*)".length) : line;
    const split = row.indexOf("|");
    const pkg = split === -1 ? row : row.slice(0, split);
    const features = split === -1 ? "" : row.slice(split + 1);
    graph.set(pkg.split(" ")[0] ?? "", new Set(features.split(",")));
  }
  return graph;
}

/** Read Cargo's target-specific, feature-unified production graph. */
export function dependencyFeatures(selection: readonly string[], root = ROOT): FeatureGraph {
  const command = [
    "cargo",
    "tree",
    "--locked",
    "--edges",
    "normal,build",
    "--prefix",
    "none",
    "--format",
    "{p}|{f}",
    ...selection,
  ];
  const result = Bun.spawnSync(command, { cwd: root, stdout: "pipe", stderr: "inherit" });
  if (result.exitCode !== 0) {
    throw new Error(`${command.join(" ")} exited with ${String(result.exitCode)}`);
  }
  return parseTree(result.stdout.toString());
}

export interface Graphs {
  readonly full: FeatureGraph;
  readonly lean: FeatureGraph;
  readonly mcp: FeatureGraph;
}

export function readGraphs(root = ROOT): Graphs {
  return {
    full: dependencyFeatures(["-p", "phux"], root),
    lean: dependencyFeatures(["-p", "phux", "-p", "phux-mcp", "--no-default-features"], root),
    mcp: dependencyFeatures(["-p", "phux-mcp"], root),
  };
}

function enables(graph: FeatureGraph, pkg: string, feature: string, label: string): boolean {
  const features = graph.get(pkg);
  if (features === undefined) throw new Error(`${label}: ${pkg} is not in the production graph`);
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
