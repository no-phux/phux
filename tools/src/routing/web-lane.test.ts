// The web lane's own checks. workflows.test.ts is at the file-size ratchet,
// so the split and its cache policy are asserted here.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { capture } from "../lib/process.ts";
import { repoPath } from "../lib/repo.ts";
import { list, str, table } from "../lib/value.ts";
import { action, job, needs, run, steps, uses, withInput, workflow } from "../policy/workflows.ts";
import { LANES, LANE_RECIPES } from "./plan.ts";

const WEB_CHECKS = [
  'if [[ "${PHUX_WEB_ENGINE:-0}" == 1 ]]; then bash clients/phux-vt-web/scripts/build-vt-wasm.sh --check; fi',
  "node --test clients/phux-vt-web/scripts/test-vt-wasm.mjs",
  "(cd clients/phux-vt-web && wasm-pack test --node --locked)",
  "(cd clients/phux-web && wasm-pack test --node --locked)",
  "(cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg --locked)",
  "bun tools/main.ts web-browser",
];

test("the web lane is two jobs with the lane's display names", () => {
  const ci = workflow("ci.yml");
  const names = ["web-vt", "web-client"].map((id) => str(job(ci, id)["name"]) ?? "");
  expect(names.toSorted()).toEqual(["web (client + browser)", "web (vt)"]);
});

test("just web-check runs both halves in the lane order", () => {
  const recipes = table(
    table(JSON.parse(capture(["just", "--dump", "--dump-format", "json"], { cwd: repoPath(".") })))[
      "recipes"
    ],
  );
  const dependencies = list(table(recipes["web-check"])["dependencies"]).map((dep) =>
    str(table(dep)["recipe"]),
  );
  expect(dependencies).toEqual(LANE_RECIPES.web.map(([recipe]) => recipe));
  const commands = (recipe: string) =>
    list(table(recipes[recipe])["body"])
      .map((line) => list(line).map(String).join(""))
      .filter((line) => line !== "#!/usr/bin/env bash" && line !== "set -euo pipefail");
  expect(LANE_RECIPES.web.flatMap(([recipe]) => commands(recipe))).toEqual(WEB_CHECKS);
});

test("only the browser client realizes Chromium", () => {
  const setup = steps(action("setup-web-cache")).map(run).join("\n");
  expect(setup).toContain("nix develop -c bash");
  expect(setup).not.toContain("nix develop .#browser");
  for (const file of ["ci.yml", "cache-warm.yml"]) {
    const current = workflow(file);
    expect(steps(job(current, "web-vt")).map(run).join("\n")).toContain(
      "nix develop -c just web-vt-check",
    );
    expect(steps(job(current, "web-client")).map(run).join("\n")).toContain(
      "nix develop .#browser -c just web-client-check",
    );
  }
});

test("the web jobs cache cargo dependencies and save them only from main", () => {
  const pin = steps(action("setup-rust-lane"))
    .map(uses)
    .find((used) => used.startsWith("Swatinem/rust-cache@"));
  const step = steps(action("setup-web-cache")).find((candidate) =>
    uses(candidate).startsWith("Swatinem/rust-cache@"),
  );
  expect(uses(step ?? {})).toBe(pin ?? "");
  const saveIf = str(withInput(step ?? {}, "save-if")) ?? "";
  expect(saveIf).toContain("github.event_name == 'push'");
  expect(saveIf).toContain("github.ref == 'refs/heads/main'");
  expect(str(withInput(step ?? {}, "shared-key"))).toContain("inputs.cache-key");
  expect(str(withInput(step ?? {}, "shared-key"))).toContain("steps.web-cache.outputs.flake");
  expect(withInput(step ?? {}, "cache-all-crates")).toBeUndefined();
  expect(withInput(step ?? {}, "cache-workspace-crates")).toBeUndefined();

  const ci = workflow("ci.yml");
  for (const [id, key, workspace] of [
    ["web-vt", "phux-web-vt", "clients/phux-vt-web"],
    ["web-client", "phux-web-client", "clients/phux-web"],
  ] as const) {
    const setup = steps(job(ci, id)).find(
      (candidate) => uses(candidate) === "./.github/actions/setup-web-cache",
    );
    expect([id, str(withInput(setup ?? {}, "cache-key"))]).toEqual([id, key]);
    expect(str(withInput(setup ?? {}, "workspaces"))).toContain(workspace);
  }
  const client = steps(job(ci, "web-client")).find(
    (candidate) => uses(candidate) === "./.github/actions/setup-web-cache",
  );
  expect(str(withInput(client ?? {}, "workspaces"))).toContain("target/web-native");
});

test("both web halves fail the aggregate on failure or cancellation", () => {
  const gate = run(
    steps(job(workflow("ci.yml"), "ci")).find((step) =>
      run(step).includes("scripts/ci/ci-aggregate.sh"),
    ) ?? {},
  );
  for (const id of ["web-vt", "web-client"]) {
    for (const result of ["success", "failure", "cancelled"]) {
      const exit = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", gate], {
        cwd: repoPath("."),
        env: {
          ...process.env,
          EVENT_NAME: "pull_request",
          RESULTS: JSON.stringify({
            ...Object.fromEntries(
              needs(job(workflow("ci.yml"), "ci")).map((id) => [id, { result: "skipped" }]),
            ),
            plan: {
              result: "success",
              outputs: {
                ...Object.fromEntries(
                  [...LANES, "rune_qualify", "release_qualify"].map((key) => [key, "false"]),
                ),
                web: "true",
              },
            },
            "web-vt": { result: "success" },
            "web-client": { result: "success" },
            [id]: { result },
          }),
        },
        stdout: "ignore",
      }).exitCode;
      expect([id, result, exit]).toEqual([id, result, result === "success" ? 0 : 1]);
    }
  }
});

test("the new web cache series participate in bounded repository retention", () => {
  const prune = readFileSync(repoPath("scripts/ci/prune-build-cache.sh"), "utf8");
  for (const prefix of ["v0-rust-phux-web-vt-", "v0-rust-phux-web-client-"]) {
    expect(prune).toContain(prefix);
  }
});
