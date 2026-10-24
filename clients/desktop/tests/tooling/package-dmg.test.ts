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
import { installInstructions, packageDmg } from "../../scripts/package-dmg.ts";

test("drag-to-Applications instructions state platform and separate runtime requirements", () => {
  expect(installInstructions).toContain("Drag Phux.app onto Applications");
  expect(installInstructions).toContain("Apple silicon and macOS 27");
  expect(installInstructions).toContain("https://phux.sh/install | sh");
  expect(installInstructions).toContain("does not replace your CLI");
});

test.skipIf(process.platform !== "darwin")(
  "DMG mounts read-only with the app, Applications link and install guide",
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
      expect(readFileSync(join(mount, "Install Phux.txt"), "utf8")).toBe(installInstructions);
      expect(() => writeFileSync(join(mount, "unexpected"), "no")).toThrow();
    } finally {
      if (mounted) expect(Bun.spawnSync(["hdiutil", "detach", mount]).exitCode).toBe(0);
      rmSync(root, { recursive: true, force: true });
    }
    expect(existsSync(root)).toBe(false);
  },
  120_000,
);
