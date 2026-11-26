// Every public native build surface selects its portable CPU floor. This is
// the static half; packaging/test-release-cpu-baselines.sh runs the real
// release builder against a fake cargo.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";

const REQUIRED: Record<string, string[]> = {
  ".github/workflows/build-cli.yml": [
    'bash packaging/build-release-binaries.sh "${{ matrix.target }}"',
  ],
  // The mobile xcframework: an explicit Rust floor per Apple target, the
  // engine on baseline, and no inherited RUSTFLAGS reaching a slice.
  "packaging/build-mobile-ffi-xcframework.sh": [
    "target-cpu=apple-a7",
    "target-cpu=apple-a12",
    "target-cpu=apple-m1",
    "LIBGHOSTTY_VT_SYS_CPU=baseline",
    "unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS",
  ],
  ".github/workflows/ffi-xcframework.yml": [
    "bash packaging/build-mobile-ffi-xcframework.sh --full",
  ],
};

function baselineFailures(read: (path: string) => string): string[] {
  return Object.entries(REQUIRED).flatMap(([path, needles]) => {
    const text = read(path);
    return needles
      .filter((needle) => !text.includes(needle))
      .map((needle) => `release CPU baseline drift: ${path} is missing ${needle}`);
  });
}

test("release and mobile builds select their portable CPU floor", () => {
  expect(baselineFailures((path) => readFileSync(repoPath(path), "utf8"))).toEqual([]);
});

test("a build surface that drops its floor is named", () => {
  const real = (path: string) => readFileSync(repoPath(path), "utf8");
  const xcframework = "packaging/build-mobile-ffi-xcframework.sh";
  const read = (path: string) =>
    path === xcframework ? real(path).replaceAll("target-cpu=apple-a12", "") : real(path);
  expect(baselineFailures(read)).toEqual([
    `release CPU baseline drift: ${xcframework} is missing target-cpu=apple-a12`,
  ]);
});
