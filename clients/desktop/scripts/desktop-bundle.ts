import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { BunPlugin } from "bun";
import solidPlugin from "@gpuix/solid/bun-plugin";

const desktop = resolve(import.meta.dir, "..");
const source = resolve(desktop, "toolchain/gpuix");

/** Bundle the verified patched framework, including its shared JS window fences. */
export const desktopFrameworkPlugin: BunPlugin = {
  name: "phux-verified-framework",
  setup(build) {
    build.onResolve({ filter: /^@gpuix\/(native|solid)(\/|$)/ }, ({ path }) => ({
      path: fileURLToPath(
        import.meta.resolve(path, resolve(source, "packages/solid/package.json")),
      ),
    }));
    // Keep application and source-workspace imports on the same reactive owner.
    build.onResolve({ filter: /^solid-js(\/|$)/ }, ({ path }) => ({
      path: fileURLToPath(import.meta.resolve(path, resolve(desktop, "package.json"))),
    }));
  },
};

/** Verify source/patch hashes before rebuilding the JS artifacts used by bundles. */
export function prepareDesktopFramework(): void {
  run(["python3", resolve(desktop, "scripts/bootstrap-source.py")], desktop);
  run([process.execPath, "run", "build:js"], resolve(source, "packages/native"));
  run([process.execPath, "run", "build"], resolve(source, "packages/solid"));
}

function run(command: string[], cwd: string): void {
  const result = Bun.spawnSync(command, { cwd, stdout: "inherit", stderr: "inherit" });
  if (result.exitCode !== 0) throw new Error(`Framework preparation failed: ${command.join(" ")}`);
}

export function buildDesktopBundle(entrypoint: string, outdir: string) {
  return Bun.build({
    entrypoints: [entrypoint],
    outdir,
    target: "bun",
    plugins: [desktopFrameworkPlugin, solidPlugin],
  });
}
