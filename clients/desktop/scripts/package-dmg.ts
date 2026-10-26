/** A read-only drag-to-Applications image; signing/notarization is a release step. */
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";

export const installInstructions = `Install Phux
============

Drag Phux.app onto Applications, then open Phux from Applications.
Eject this disk image after copying. Do not run Phux from the disk image.

Requires Apple silicon and macOS 26 or later.
Phux includes its CLI and server. On first launch it installs the bundled CLI
in ~/.local/bin/phux if no CLI is already installed, then starts or reuses
your local server. No separate download is required. If Terminal cannot find
phux, choose Add to PATH in the app, or copy the command it shows, then open
a new Terminal window. An existing CLI is never replaced, and running
sessions are not restarted.
If startup fails, the window explains why and offers Retry connection.

Help: https://phux.sh
`;

// Retried historical releases use this harness too; describe the bundle actually copied.
export const legacyInstallInstructions = `Install Phux
============

Drag Phux.app onto Applications, then open Phux from Applications.
Eject this disk image after copying. Do not run Phux from the disk image.

Requires Apple silicon and macOS 26 or later.
This older release uses a separately installed phux CLI. If needed, run:

    curl -fsSL https://phux.sh/install | sh

Then reopen Phux. An existing CLI and running sessions are not replaced.
Help: https://phux.sh
`;

export function packageDmg(appPath: string, outputPath: string): void {
  const app = resolve(appPath);
  const output = resolve(outputPath);
  if (!existsSync(join(app, "Contents/Info.plist"))) {
    throw new Error(`Not an app bundle: ${app}`);
  }
  if (existsSync(output)) throw new Error(`Refusing to overwrite ${output}`);
  const work = mkdtempSync(join(tmpdir(), "phux-dmg-"));
  try {
    run(["ditto", app, join(work, "Phux.app")]);
    symlinkSync("/Applications", join(work, "Applications"));
    writeFileSync(
      join(work, "Install Phux.txt"),
      existsSync(join(app, "Contents/MacOS/phux"))
        ? installInstructions
        : legacyInstallInstructions,
    );
    mkdirSync(dirname(output), { recursive: true });
    run([
      "hdiutil",
      "create",
      "-volname",
      "Phux",
      "-srcfolder",
      work,
      "-format",
      "UDZO",
      "-fs",
      "HFS+",
      output,
    ]);
    run(["hdiutil", "verify", output]);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

function run(command: string[]): void {
  const result = Bun.spawnSync(command, { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new Error(`${command[0]} failed: ${result.stderr.toString().trim()}`);
  }
}

if (import.meta.main) {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: { app: { type: "string" }, output: { type: "string" } },
  });
  if (!values.app || !values.output)
    throw new Error("Usage: package-dmg.ts --app APP --output DMG");
  packageDmg(values.app, values.output);
}
