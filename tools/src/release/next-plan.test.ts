import { expect, test } from "bun:test";
import { reason } from "./next-plan.ts";

test.each<[string, string[], boolean]>([
  ["phux", ["crates/phux/src/main.rs"], true],
  ["phux", ["scripts/pack-release.sh"], true],
  ["phux", [".github/workflows/build-cli.yml"], true],
  ["phux", ["clients/desktop/native/src/main.rs"], false],
  ["phux", ["docs/RELEASING.md"], false],
  ["desktop", ["clients/desktop/native/src/main.rs"], true],
  ["desktop", [".github/workflows/desktop-release.yml"], true],
  ["desktop", ["crates/phux-protocol/src/lib.rs"], true],
  ["desktop", [".github/workflows/build-cli.yml"], false],
  ["desktop", ["integrations/pi/src/index.ts"], false],
])("%s for %p rebuilds: %p", (product, paths, rebuild) => {
  expect(reason(product as "phux" | "desktop", paths) !== undefined).toBe(rebuild);
});
