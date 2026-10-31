/**
 * Print the third-party notice Phux Rune ships: the desktop's generator over
 * this workspace's Cargo graph, so both products list crates the same way.
 *
 * Usage: bun clients/rune/scripts/notices.ts TARGET > THIRD-PARTY-NOTICES.txt
 */
import { dirname, resolve } from "node:path";
import {
  canonicalLicenses,
  noticeTexts,
  renderNotices,
  shippedCrates,
} from "../../desktop/scripts/notices.ts";

const [target] = process.argv.slice(2);
if (target === undefined) throw new Error("usage: notices.ts TARGET");
const manifest = resolve(dirname(import.meta.path), "../Cargo.toml");
const metadata = Bun.spawnSync(
  [
    "cargo",
    "metadata",
    "--locked",
    "--format-version",
    "1",
    "--filter-platform",
    target,
    "--manifest-path",
    manifest,
  ],
  { stderr: "inherit" },
);
if (!metadata.success) throw new Error("cargo metadata failed");
const canonical = canonicalLicenses();
const crates = shippedCrates(metadata.stdout.toString());
const read = (crate: (typeof crates)[number]) => noticeTexts(crate, canonical);
process.stdout.write(renderNotices(crates, read, "Phux Rune"));
