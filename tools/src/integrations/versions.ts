// Agent integration release contract: every package version agrees with
// Release Please, its lockfile and its marketplace entry; the published
// packages stay publishable with provenance; and the neutral runtime is not
// implemented by one harness and borrowed by another.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Expectations } from "../lib/expect.ts";
import { ROOT } from "../lib/repo.ts";
import { list, str, strings, table, type Table } from "../lib/value.ts";

const EXACT = /^\d+\.\d+\.\d+$/;
const PI_IMPORT = /(?:@phux\/pi|integrations\/pi|\.\.\/\.\.\/pi\/)/;

class Repo {
  constructor(readonly root: string) {}

  json(path: string): Table {
    return table(JSON.parse(readFileSync(join(this.root, path), "utf8")));
  }
}

function at(document: Table, ...path: string[]): unknown {
  let current: unknown = document;
  for (const key of path) current = table(current)[key];
  return current;
}

function plugin(marketplace: Table, name: string): Table | undefined {
  const found = list(marketplace["plugins"]).find((entry) => table(entry)["name"] === name);
  return found === undefined ? undefined : table(found);
}

function lockedVersions(repo: Repo, released: Table, expect: Expectations): void {
  for (const directory of ["integrations/pi", "integrations/claude"]) {
    const manifest = repo.json(join(directory, "package.json"));
    const lock = repo.json(join(directory, "package-lock.json"));
    expect.equal(
      manifest["version"],
      released[directory],
      `${directory} package version must match release-please`,
    );
    expect.equal(
      lock["version"],
      manifest["version"],
      `${directory} lockfile version must match package.json`,
    );
    expect.equal(
      at(lock, "packages", "", "version"),
      manifest["version"],
      `${directory} lockfile root package must match package.json`,
    );
  }
}

function marketplacePlugin(
  repo: Repo,
  name: "Claude" | "Grok",
  paths: { directory: string; manifest: string; marketplace: string },
  expect: Expectations,
): Table {
  const pkg = repo.json(join(paths.directory, "package.json"));
  const manifest = repo.json(paths.manifest);
  const entry = plugin(repo.json(paths.marketplace), "phux");
  expect.that(entry !== undefined, `${name} marketplace must contain the phux plugin`);
  expect.equal(
    manifest["version"],
    pkg["version"],
    `${name} plugin manifest version must match package.json`,
  );
  if (entry !== undefined) {
    expect.equal(
      entry["version"],
      pkg["version"],
      `${name} marketplace version must match package.json`,
    );
    expect.equal(
      entry["source"],
      `./${paths.directory}`,
      `${name} marketplace source must remain repository-relative`,
    );
  }
  return pkg;
}

function publishable(pkg: Table, label: string, expect: Expectations): void {
  expect.equal(pkg["private"], undefined, `${label} must remain publishable`);
  expect.equal(at(pkg, "publishConfig", "access"), "public", `${label} publishes publicly`);
  expect.equal(at(pkg, "publishConfig", "provenance"), true, `${label} publishes with provenance`);
}

function openCode(repo: Repo, released: Table, expect: Expectations): void {
  const opencode = repo.json("integrations/opencode-v2/package.json");
  expect.equal(
    opencode["name"],
    "@phux/opencode",
    "OpenCode V2 plugin publishes under the existing npm package",
  );
  publishable(opencode, "OpenCode plugin", expect);
  expect.equal(
    opencode["version"],
    released["integrations/opencode-v2"],
    "integrations/opencode-v2 package version must match release-please",
  );
  expect.that(
    EXACT.test(str(at(opencode, "dependencies", "@opencode/plugin")) ?? ""),
    "OpenCode V2 plugin API must be pinned exactly",
  );
}

function omp(repo: Repo, expect: Expectations): Table {
  const pkg = repo.json("integrations/omp/package.json");
  publishable(pkg, "OMP extension", expect);
  expect.that(
    EXACT.test(str(at(pkg, "devDependencies", "@oh-my-pi/pi-coding-agent")) ?? ""),
    "OMP SDK must be pinned exactly",
  );
  expect.equal(at(pkg, "omp", "extensions"), ["./dist/index.js"], "OMP loads its built extension");
  expect.that(strings(pkg["files"]).includes("skills"), "OMP must ship its native-tool skill");
  expect.equal(
    pkg["dependencies"] ?? {},
    {},
    "OMP bundles the neutral runtime without runtime dependencies",
  );
  return pkg;
}

function piAndRuntime(repo: Repo, expect: Expectations): void {
  const pi = repo.json("integrations/pi/package.json");
  publishable(pi, "Pi extension", expect);
  expect.equal(
    at(pi, "dependencies", "@phux/integration-runtime"),
    "file:../runtime",
    "Pi depends on the in-repository runtime",
  );
  expect.that(
    strings(pi["bundledDependencies"]).includes("@phux/integration-runtime"),
    "Pi must carry the private runtime in its independently installable artifact",
  );
  const runtime = repo.json("integrations/runtime/package.json");
  const runtimeLock = repo.json("integrations/runtime/package-lock.json");
  expect.equal(
    runtime["private"],
    true,
    "the shared runtime is an implementation module, not a public product",
  );
  expect.equal(
    runtimeLock["version"],
    runtime["version"],
    "runtime lockfile version must match package.json",
  );
  expect.equal(
    at(runtimeLock, "packages", "", "version"),
    runtime["version"],
    "runtime lockfile root package must match package.json",
  );
}

function openCodeOwnsItsSource(root: string, expect: Expectations): void {
  const source = join(root, "integrations/opencode-v2/src");
  const files = readdirSync(source, { recursive: true, encoding: "utf8" });
  for (const relative of files.filter((path) => path.endsWith(".ts"))) {
    expect.that(
      !PI_IMPORT.test(readFileSync(join(source, relative), "utf8")),
      `OpenCode source must not import implementation owned by Pi: ${relative}`,
    );
  }
}

/** Every broken integration contract, as a message; empty when coherent. */
export function integrationViolations(root = ROOT): string[] {
  const repo = new Repo(root);
  const expect = new Expectations();
  const released = repo.json(".release-please-manifest.json");
  lockedVersions(repo, released, expect);
  marketplacePlugin(
    repo,
    "Claude",
    {
      directory: "integrations/claude",
      manifest: "integrations/claude/.claude-plugin/plugin.json",
      marketplace: ".claude-plugin/marketplace.json",
    },
    expect,
  );
  openCode(repo, released, expect);
  const grok = marketplacePlugin(
    repo,
    "Grok",
    {
      directory: "integrations/grok",
      manifest: "integrations/grok/.grok-plugin/plugin.json",
      marketplace: ".grok-plugin/marketplace.json",
    },
    expect,
  );
  expect.equal(
    grok["private"],
    true,
    "the Grok plugin ships from its marketplace in this repository, not npm",
  );
  const ompPackage = omp(repo, expect);
  // Release Please records Grok and OMP in the manifest at their first release.
  for (const [directory, pkg] of [
    ["integrations/grok", grok],
    ["integrations/omp", ompPackage],
  ] as const) {
    const version = released[directory];
    if (version !== undefined) {
      expect.equal(
        pkg["version"],
        version,
        `${directory} package version must match release-please`,
      );
    }
  }
  piAndRuntime(repo, expect);
  openCodeOwnsItsSource(root, expect);
  return expect.failures;
}
