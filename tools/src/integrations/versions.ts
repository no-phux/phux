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

function at(document: unknown, ...path: string[]): unknown {
  return path.reduce((value, key) => table(value)[key], document);
}

/** Every broken integration contract, as a message; empty when coherent. */
export function integrationViolations(root = ROOT): string[] {
  const json = (path: string): Table => table(JSON.parse(readFileSync(join(root, path), "utf8")));
  const pkg = (name: string) => json(`integrations/${name}/package.json`);
  const expect = new Expectations();
  const released = json(".release-please-manifest.json");

  // Grok and OMP enter the Release Please manifest at their first release.
  for (const name of ["pi", "claude", "opencode-v2", "grok", "omp"]) {
    const version = released[`integrations/${name}`];
    if (version === undefined && (name === "grok" || name === "omp")) continue;
    const message = `integrations/${name} package version must match release-please`;
    expect.equal(pkg(name)["version"], version, message);
  }
  const matches = (name: string, actual: unknown, what: string) =>
    expect.equal(actual, pkg(name)["version"], `${name} ${what} must match package.json`);
  for (const name of ["pi", "claude", "runtime"]) {
    const lock = json(`integrations/${name}/package-lock.json`);
    matches(name, lock["version"], "lockfile version");
    matches(name, at(lock, "packages", "", "version"), "lockfile root package");
  }
  for (const name of ["claude", "grok"]) {
    const dot = `.${name}-plugin`;
    const manifest = json(`integrations/${name}/${dot}/plugin.json`);
    matches(name, manifest["version"], "plugin manifest version");
    const plugins = list(json(`${dot}/marketplace.json`)["plugins"]);
    const entry = table(plugins.find((plugin) => table(plugin)["name"] === "phux"));
    matches(name, entry["version"], "marketplace phux plugin version");
    const where = `${name} marketplace source must remain repository-relative`;
    expect.equal(entry["source"], `./integrations/${name}`, where);
  }
  for (const name of ["opencode-v2", "omp", "pi"]) {
    const document = pkg(name);
    expect.equal(document["private"], undefined, `${name} must remain publishable`);
    expect.equal(at(document, "publishConfig", "access"), "public", `${name} publishes publicly`);
    const provenance = at(document, "publishConfig", "provenance");
    expect.equal(provenance, true, `${name} publishes with provenance`);
  }

  const [opencode, omp, pi] = [pkg("opencode-v2"), pkg("omp"), pkg("pi")];
  const pinned = (value: unknown) => EXACT.test(str(value) ?? "");
  const runtime = "@phux/integration-runtime";
  expect.equal(opencode["name"], "@phux/opencode", "OpenCode keeps its npm package name");
  const api = at(opencode, "dependencies", "@opencode/plugin");
  expect.that(pinned(api), "the OpenCode plugin API must be pinned exactly");
  const sdk = at(omp, "devDependencies", "@oh-my-pi/pi-coding-agent");
  expect.that(pinned(sdk), "the OMP SDK must be pinned exactly");
  expect.equal(at(omp, "omp", "extensions"), ["./dist/index.js"], "OMP loads its built extension");
  expect.that(strings(omp["files"]).includes("skills"), "OMP must ship its native-tool skill");
  expect.equal(omp["dependencies"] ?? {}, {}, "OMP bundles the runtime, with no dependencies");
  expect.equal(at(pi, "dependencies", runtime), "file:../runtime", "Pi uses the in-repo runtime");
  expect.that(strings(pi["bundledDependencies"]).includes(runtime), "Pi must bundle the runtime");
  expect.equal(pkg("runtime")["private"], true, "the shared runtime must stay private");
  expect.equal(pkg("grok")["private"], true, "Grok ships from its marketplace, not npm");

  const source = join(root, "integrations/opencode-v2/src");
  for (const file of readdirSync(source, { recursive: true, encoding: "utf8" })) {
    if (!file.endsWith(".ts")) continue;
    const borrowed = PI_IMPORT.test(readFileSync(join(source, file), "utf8"));
    expect.that(!borrowed, `OpenCode source must not import implementation owned by Pi: ${file}`);
  }
  return expect.failures;
}
