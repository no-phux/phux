import { expect, test } from "bun:test";
import {
  existsSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  installInstructions,
  legacyInstallInstructions,
  packageDmg,
  prepareLayoutTools,
} from "../../scripts/package-dmg.ts";
import { bundleRuntime, infoPlist, renderIcon } from "../../scripts/package-app.ts";

test("drag-to-Applications instructions require no separate runtime download", () => {
  expect(installInstructions).toContain("Drag Phux.app onto Applications");
  expect(installInstructions).toContain("Apple silicon and macOS 26");
  expect(installInstructions).toContain("No separate download is required");
  expect(installInstructions).toContain("choose Add to PATH in Phux");
  expect(installInstructions).toContain("~/.zshrc for the default zsh");
  expect(installInstructions).not.toContain("No separate download or Terminal command is required");
  expect(installInstructions).not.toContain("copy the command it shows");
  expect(installInstructions).toContain("An existing CLI is never replaced");
  expect(installInstructions).toContain("Retry connection");
});

test("the bundle includes the executable CLI and refuses a missing runtime", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-bundle-runtime-"));
  try {
    const source = join(root, "phux");
    const bundle = join(root, "Phux.app");
    expect(() => bundleRuntime(source, bundle)).toThrow("Missing bundled phux CLI");
    writeFileSync(source, "same-checkout runtime");
    bundleRuntime(source, bundle);
    expect(readFileSync(join(bundle, "Contents/MacOS/phux"), "utf8")).toBe("same-checkout runtime");
    expect(Bun.spawnSync(["test", "-x", join(bundle, "Contents/MacOS/phux")]).exitCode).toBe(0);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
test("the packaged bundle declares the macOS 26 compatibility floor", () => {
  expect(infoPlist("1.2.3", "a".repeat(40))).toContain(
    "<key>LSMinimumSystemVersion</key><string>26.0</string>",
  );
});

for (const bundled of [false, true]) {
  test.skipIf(process.platform !== "darwin")(
    `DMG mounts read-only with matching install guide (bundled=${bundled})`,
    () => {
      // Spaces in both source and destination exercise argument-safe packaging.
      const root = mkdtempSync(join(tmpdir(), "phux dmg test-"));
      const mount = join(root, "mounted");
      const image = join(root, "Phux.dmg");
      const app = join(root, "Phux.app");
      let mounted = false;
      try {
        expect(() => packageDmg(app, image)).toThrow("Not an app bundle");
        mkdirSync(join(app, "Contents/MacOS"), { recursive: true });
        mkdirSync(join(app, "Contents/Resources"));
        const plist = infoPlist("1.2.3", "a".repeat(40));
        writeFileSync(join(app, "Contents/Info.plist"), plist);
        copyFileSync("/usr/bin/true", join(app, "Contents/MacOS/phux-desktop"));
        renderIcon(join(app, "Contents/Resources/AppIcon.icns"));
        if (bundled) {
          copyFileSync("/usr/bin/true", join(app, "Contents/MacOS/phux"));
        }
        expect(Bun.spawnSync(["codesign", "--force", "--deep", "--sign", "-", app]).exitCode).toBe(
          0,
        );
        // macOS temporary paths can contain symlinks, including /var itself.
        const scratch = join(root, "scratch");
        const linkedScratch = join(root, "linked scratch");
        mkdirSync(scratch);
        symlinkSync(scratch, linkedScratch);
        const packaged = Bun.spawnSync(
          [
            process.execPath,
            join(import.meta.dir, "../../scripts/package-dmg.ts"),
            "--app",
            app,
            "--output",
            image,
          ],
          { env: { ...process.env, TMPDIR: linkedScratch } },
        );
        expect(packaged.exitCode, packaged.stderr.toString()).toBe(0);
        expect(() => packageDmg(app, image)).toThrow("Refusing to overwrite");
        const result = Bun.spawnSync([
          "hdiutil",
          "attach",
          "-readonly",
          "-nobrowse",
          "-mountpoint",
          mount,
          image,
        ]);
        expect(result.exitCode).toBe(0);
        mounted = true;
        expect(readFileSync(join(mount, "Phux.app/Contents/Info.plist"), "utf8")).toBe(plist);
        expect(readlinkSync(join(mount, "Applications"))).toBe("/Applications");
        expect(readFileSync(join(mount, "Install Phux.txt"), "utf8")).toBe(
          bundled ? installInstructions : legacyInstallInstructions,
        );
        const python = prepareLayoutTools(root);
        const alias = join(root, "background.alias");
        const metadata = Bun.spawnSync([
          python,
          join(import.meta.dir, "verify-dmg.py"),
          mount,
          alias,
        ]);
        expect(metadata.stderr.toString()).toBe("");
        expect(metadata.exitCode).toBe(0);
        const resolved = Bun.spawnSync([
          "xcrun",
          "swift",
          join(import.meta.dir, "verify-dmg.swift"),
          mount,
          alias,
        ]);
        expect(resolved.exitCode, resolved.stderr.toString()).toBe(0);
        const retina = Bun.spawnSync(["tiffutil", "-info", join(mount, ".background.tiff")]);
        expect(retina.exitCode).toBe(0);
        expect(retina.stdout.toString() + retina.stderr.toString()).toContain("1440");
        // Copy exactly as Finder would and verify the signature after installation.
        const installed = join(root, "Applications/Phux.app");
        expect(Bun.spawnSync(["ditto", join(mount, "Phux.app"), installed]).exitCode).toBe(0);
        expect(
          Bun.spawnSync(["codesign", "--verify", "--deep", "--strict", installed]).exitCode,
        ).toBe(0);
        expect(readFileSync(join(installed, "Contents/Resources/AppIcon.icns"))).toEqual(
          readFileSync(join(app, "Contents/Resources/AppIcon.icns")),
        );
        expect(() => writeFileSync(join(mount, "unexpected"), "no")).toThrow();
      } finally {
        if (mounted) expect(Bun.spawnSync(["hdiutil", "detach", mount]).exitCode).toBe(0);
        rmSync(root, { recursive: true, force: true });
      }
      expect(existsSync(root)).toBe(false);
    },
    120_000,
  );
}
