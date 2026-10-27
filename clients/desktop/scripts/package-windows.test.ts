import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createWindowsBundle,
  archiveWindowsBundle,
  verifyWindowsExecutable,
  windowsAssetName,
  WINDOWS_TARGET,
} from "./package-windows.ts";

test("Windows assets are desktop-only and immutable on next", () => {
  expect(windowsAssetName("1.2.3", "a".repeat(40), "stable")).toBe(
    "phux-desktop-1.2.3-windows-x64.zip",
  );
  expect(windowsAssetName("1.2.3", "a".repeat(40), "next")).toBe(
    `phux-desktop-next.${"a".repeat(40)}-windows-x64.zip`,
  );
  expect(() => windowsAssetName("1.2.3", "main", "next")).toThrow();
});

test("Windows packaging checks PE architecture and ships app-local runtime and provenance", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-windows-package-"));
  try {
    const binary = join(root, "app.exe");
    const pe = Buffer.alloc(256);
    pe.write("MZ");
    pe.writeUInt32LE(64, 0x3c);
    pe.write("PE\0\0", 64);
    pe.writeUInt16LE(0x8664, 68);
    pe.writeUInt16LE(0x20b, 88);
    pe.writeUInt16LE(2, 156);
    writeFileSync(binary, pe);
    const runtime = join(root, "crt");
    mkdirSync(runtime);
    for (const name of ["vcruntime140.dll", "msvcp140.dll"])
      writeFileSync(join(runtime, name), "runtime fixture");
    const output = join(root, "out");
    createWindowsBundle({
      binary,
      runtime,
      output,
      version: "1.2.3",
      sha: "b".repeat(40),
      channel: "stable",
      notices: "fixture notices",
    });
    expect(
      JSON.parse(
        readFileSync(join(output, "phux-desktop/share/phux-desktop/release.json"), "utf8"),
      ),
    ).toMatchObject({ target: WINDOWS_TARGET, sha: "b".repeat(40) });
    expect(readFileSync(join(output, "phux-desktop/vcruntime140.dll"), "utf8")).toBe(
      "runtime fixture",
    );
    const archive = join(root, "desktop.zip");
    archiveWindowsBundle(output, archive);
    expect(readFileSync(archive).toString("ascii", 0, 2)).toBe("PK");
    const listing = Bun.spawnSync(
      process.platform === "linux"
        ? ["unzip", "-Z1", archive]
        : [process.platform === "darwin" ? "/usr/bin/tar" : "tar", "-tf", archive],
    );
    expect(listing.exitCode).toBe(0);
    expect(listing.stdout.toString()).toContain("phux-desktop/install.ps1");
    expect(listing.stdout.toString()).toContain("phux-desktop/share/phux-desktop/release.json");
    pe.writeUInt16LE(0xaa64, 68);
    writeFileSync(binary, pe);
    expect(() => verifyWindowsExecutable(binary)).toThrow("AMD64");
    pe.writeUInt32LE(0xffffffff, 0x3c);
    writeFileSync(binary, pe);
    expect(() => verifyWindowsExecutable(binary)).toThrow("header");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
