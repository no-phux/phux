import { describe, expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { reusedMacos, runeMacosPlan } from "./rune-macos.ts";

const classifier = new Classifier();

describe("the Rune iOS simulator leg", () => {
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const direct = classifier.classify(["clients/rune/src/main.rs"]);
  const shared = classifier.classify(["Cargo.toml"]);

  test("a pull request never claims Apple coverage, even when native inputs moved", () => {
    expect(runeMacosPlan(closure, "pull_request")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("direct and shared Rune inputs still start no Apple runner on a PR", () => {
    for (const c of [direct, shared, classifier.classify([])]) {
      expect(runeMacosPlan(c, "pull_request").macos).toBe(false);
    }
  });

  test("main policy always runs both legs, and nothing without Rune does", () => {
    const run = runeMacosPlan(closure, "push");
    expect(run).toMatchObject({ macos: true, ios: true });
    expect(run.coverage.split(",")).toEqual(["macos", "rune_ios"]);
    const docs = classifier.classify(["docs/SETUP.md"]);
    expect(runeMacosPlan(docs, "push")).toMatchObject({
      macos: false,
      ios: false,
      coverage: "",
    });
  });

  test("a main push never reuses a pull request receipt for macOS", () => {
    const main = runeMacosPlan(closure, "push").coverage;
    expect(main.split(",")).toContain("rune_ios");
    expect(runeMacosPlan(closure, "pull_request").coverage).not.toBe(main);
    expect(runeMacosPlan(closure, "pull_request").coverage).not.toContain("macos");
  });

  test("a reused main tree launches no macOS leg and keeps the coverage it matched", () => {
    const planned = runeMacosPlan(direct, "push");
    expect(reusedMacos(planned)).toMatchObject({ macos: false, ios: false });
    expect(planned.coverage).toContain("rune_ios");
  });

  test("a nightly or dispatch of the whole tree runs both legs", () => {
    const full = classifier.classify([]);
    expect(runeMacosPlan(full, "push")).toMatchObject({ macos: true, ios: true });
  });

  test.each([
    "clients/rune/Cargo.toml",
    "clients/rune/Cargo.lock",
    "clients/rune/ios/Bridge.h",
    "clients/rune/ios/Tailnet/Package.resolved",
    ".github/actions/rune-host/action.yml",
    ".github/workflows/rune-macos.yml",
    ".config/zig-toolchain.json",
    "scripts/lib/dev-toolchain.sh",
    "scripts/lib/apple-toolchain-env.sh",
    "crates/phux-client-runtime/build.rs",
  ])("native build input %s retains both queue/main legs", (path) => {
    expect(runeMacosPlan(classifier.classify([path]), "push")).toMatchObject({
      macos: true,
      ios: true,
      coverage: "macos,rune_ios",
    });
  });
});
