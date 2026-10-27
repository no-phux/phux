/** A read-only drag-to-Applications image; signing/notarization is a release step. */
import {
  constants,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { desktopBrand } from "./brand.ts";
import { renderIcon } from "./package-app.ts";
import { repoRoot } from "./host.ts";

export const installInstructions = `Install Phux
============

Drag Phux.app onto Applications, then open Phux from Applications.
Eject this disk image after copying. Do not run Phux from the disk image.

Requires Apple silicon and macOS 26 or later.
Phux includes its CLI and server. On first launch it installs the bundled CLI
in ~/.local/bin/phux if no CLI is already installed, then starts or reuses
your local server. No separate download is required. If Terminal cannot find
phux, choose Add to PATH in Phux. It appends one line to your shell profile
(~/.zshrc for the default zsh). Then open a new Terminal window.
An existing CLI is never replaced, and running sessions are not restarted.
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
  const stage = join(work, "stage");
  const mount = join(work, "mounted");
  let mounted = false;
  try {
    run(["python3", join(repoRoot, "scripts/brand.py"), "check", "--root", repoRoot]);
    mkdirSync(stage);
    run(["ditto", app, join(stage, "Phux.app")]);
    symlinkSync("/Applications", join(stage, "Applications"));
    writeFileSync(
      join(stage, "Install Phux.txt"),
      existsSync(join(app, "Contents/MacOS/phux"))
        ? installInstructions
        : legacyInstallInstructions,
    );
    const python = prepareLayoutTools(work);
    renderInstallerArtwork(work, stage);
    mkdirSync(dirname(output), { recursive: true });
    run([
      "hdiutil",
      "create",
      "-volname",
      "Phux",
      "-srcfolder",
      stage,
      "-format",
      "UDRW",
      "-fs",
      "HFS+",
      join(work, "writable.dmg"),
    ]);
    run([
      "hdiutil",
      "attach",
      "-nobrowse",
      "-noautoopen",
      "-mountpoint",
      mount,
      join(work, "writable.dmg"),
    ]);
    mounted = true;
    run([python, join(import.meta.dir, "dmg-layout.py"), mount]);
    run(["xcrun", "SetFile", "-a", "C", mount]);
    run(["hdiutil", "detach", mount]);
    mounted = false;
    // Convert privately, then publish only a complete verified image.
    const compressed = join(work, "Phux.dmg");
    run(["hdiutil", "convert", join(work, "writable.dmg"), "-format", "UDZO", "-o", compressed]);
    run(["hdiutil", "verify", compressed]);
    copyFileSync(compressed, output, constants.COPYFILE_EXCL);
  } finally {
    // Never remove a directory while our image is still mounted inside it.
    if (mounted) run(["hdiutil", "detach", mount]);
    rmSync(work, { recursive: true, force: true });
  }
}

export function prepareLayoutTools(work: string): string {
  const environment = join(work, "python");
  run(["python3", "-m", "venv", environment]);
  const python = join(environment, "bin/python3");
  run([
    python,
    "-m",
    "pip",
    "install",
    "--disable-pip-version-check",
    "--require-hashes",
    "--only-binary=:all:",
    "-r",
    join(import.meta.dir, "../packaging/dmg-requirements.txt"),
  ]);
  return python;
}

function renderInstallerArtwork(work: string, stage: string): void {
  run([
    "xcrun",
    "swift",
    join(import.meta.dir, "dmg-artwork.swift"),
    join(repoRoot, "docs/assets/logo.png"),
    desktopBrand().iconPng,
    work,
  ]);
  run([
    "tiffutil",
    "-cathidpicheck",
    join(work, "background.png"),
    join(work, "background@2x.png"),
    "-out",
    join(stage, ".background.tiff"),
  ]);
  renderIcon(join(stage, ".VolumeIcon.icns"), join(work, "VolumeIcon.png"));
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
