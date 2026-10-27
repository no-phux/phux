import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

test("D3D12 allocator and wgpu share the same Windows COM interface types", () => {
  const lock = Bun.TOML.parse(
    readFileSync(new URL("../../../clients/desktop/native/Cargo.lock", import.meta.url), "utf8"),
  ) as { package: { name: string; dependencies?: string[] }[] };
  const windowsDependency = (name: string) =>
    lock.package
      .find((pkg) => pkg.name === name)
      ?.dependencies?.find((dep) => dep.startsWith("windows "));

  // gpu-allocator's broad >=0.53,<=0.62 constraint otherwise reuses GPUI's
  // older COM bindings. The packages compile separately but their heap/device
  // types are incompatible when wgpu-hal calls into the allocator on Windows.
  expect(windowsDependency("gpu-allocator")).toBe(windowsDependency("wgpu-hal"));
  expect(windowsDependency("wgpu-hal")).toBe("windows 0.62.2");
});
