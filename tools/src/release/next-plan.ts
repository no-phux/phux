// Which products next.yml rebuilds for a green main commit: each one whose
// inputs moved since the commit next.json names for it.
import { writeOutputs } from "../lib/actions.ts";
import { describe, Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { Classifier, type Classification } from "../routing/classify.ts";
import { diffPaths } from "../routing/detect.ts";
import { matchesAny } from "../routing/routes.ts";
import type { Product } from "./manifest.ts";
import { nextSha } from "./release-manifest.ts";

/** Release plumbing the classifier sees as compile-free, per product. */
const BUILDERS: Record<Product, string[]> = {
  phux: [
    ".github/workflows/build-cli.yml",
    "scripts/{build-release-binaries,pack-release,check-binary-portability}.sh",
    "scripts/ci/setup-linux-release-userspace.sh",
  ],
  desktop: [".github/workflows/desktop-release.yml"],
};

const SURFACES: Record<Product, (c: Classification) => boolean> = {
  phux: (c) => c.needed.has("phux") || c.needed.has("native"),
  desktop: (c) => c.needed.has("phux") || c.needed.has("native") || c.needed.has("desktop"),
};

/** Why `product` needs a build, or undefined when `paths` cannot change it. */
export function reason(product: Product, paths: readonly string[]): string | undefined {
  const builder = paths.find((path) => matchesAny(path, BUILDERS[product]));
  if (builder !== undefined) return `${builder} changed`;
  return SURFACES[product](new Classifier().classify(paths)) ? "its sources changed" : undefined;
}

/** Whether a product advances to SHA; older or divergent CI completions never roll it back. */
export function needsBuild(product: Product, previous: string, sha: string, cwd = ROOT): boolean {
  if (previous === sha) return log(product, "already published", false);
  try {
    git(["cat-file", "-e", `${previous}^{commit}`], cwd);
  } catch {
    return log(product, "no usable previous commit", true);
  }
  if (!advances(previous, sha, cwd)) {
    return log(product, "candidate does not advance the published commit", false);
  }
  let why: string | undefined;
  try {
    why = reason(product, diffPaths(previous, sha, cwd));
  } catch (error) {
    return log(product, `classification failed: ${describe(error)}`, true);
  }
  return log(product, why ?? "unchanged", why !== undefined);
}

/** Main is linear, but its CI jobs can finish in any order. */
function advances(previous: string, sha: string, cwd: string): boolean {
  try {
    return git(["merge-base", previous, sha], cwd).trim() === previous;
  } catch (error) {
    console.error(`Cannot establish forward history: ${describe(error)}`);
    return false;
  }
}

function log(product: Product, why: string, build: boolean): boolean {
  console.error(`${product}: ${why}`);
  return build;
}

/** next.yml's gate: SHA in, `phux` and `desktop` booleans out. */
export function main(sha: string): number {
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new Failure(`not a git SHA: ${sha}`);
  writeOutputs({
    sha,
    phux: needsBuild("phux", nextSha("phux"), sha),
    desktop: needsBuild("desktop", nextSha("desktop"), sha),
  });
  return 0;
}
