import { expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readlinkSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  installInstructions,
  legacyInstallInstructions,
  packageDmg,
} from "../../scripts/package-dmg.ts";
import { bundleRuntime, infoPlist } from "../../scripts/package-app.ts";

test("drag-to-Applications instructions require no separate runtime download", () => {
  expect(installInstructions).toContain("Drag Phux.app onto Applications");
  expect(installInstructions).toContain("Apple silicon and macOS 26");
  expect(installInstructions).toContain("No separate download is required");
  expect(installInstructions).toContain("Add to PATH");
  expect(installInstructions).not.toContain("No separate download or Terminal command is required");
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
      const root = mkdtempSync(join(tmpdir(), "phux-dmg-test-"));
      const mount = join(root, "mounted");
      const image = join(root, "Phux.dmg");
      const app = join(root, "Phux.app");
      let mounted = false;
      try {
        expect(() => packageDmg(app, image)).toThrow("Not an app bundle");
        mkdirSync(join(app, "Contents"), { recursive: true });
        writeFileSync(join(app, "Contents/Info.plist"), "fixture bundle");
        if (bundled) {
          mkdirSync(join(app, "Contents/MacOS"));
          writeFileSync(join(app, "Contents/MacOS/phux"), "fixture runtime", { mode: 0o755 });
        }
        packageDmg(app, image);
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
        expect(readFileSync(join(mount, "Phux.app/Contents/Info.plist"), "utf8")).toBe(
          "fixture bundle",
        );
        expect(readlinkSync(join(mount, "Applications"))).toBe("/Applications");
        expect(readFileSync(join(mount, "Install Phux.txt"), "utf8")).toBe(
          bundled ? installInstructions : legacyInstallInstructions,
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
