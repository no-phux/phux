import { desktopBrand } from "../../scripts/brand.ts";
import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createLinuxBundle, linuxAssetName } from "../../scripts/package-linux.ts";

test("Linux artifact names include target architecture and next revision", () => {
  expect(linuxAssetName("1.2.3", "a".repeat(40), "x86_64-unknown-linux-gnu", "stable")).toBe(
    "phux-desktop-1.2.3-linux-x64.tar.gz",
  );
  expect(linuxAssetName("1.2.3", "a".repeat(40), "aarch64-unknown-linux-gnu", "next")).toBe(
    `phux-desktop-next.${"a".repeat(40)}-linux-arm64.tar.gz`,
  );
  expect(() => linuxAssetName("1.2.3", "a".repeat(40), "aarch64-apple-darwin", "stable")).toThrow();
});

test("portable bundle includes installed-version metadata, desktop integration and licenses", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-linux-package-"));
  try {
    const binary = join(root, "binary");
    const elf = Buffer.alloc(20);
    elf.write("\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf.writeUInt16LE(62, 18);
    writeFileSync(binary, elf);
    const options = {
      binary,
      output: root,
      version: "1.2.3",
      sha: "a".repeat(40),
      target: "x86_64-unknown-linux-gnu",
      channel: "stable" as const,
      notices: "test notices",
    };
    createLinuxBundle(options);
    const bundle = join(root, "phux-desktop");
    const read = (path: string) => readFileSync(join(bundle, path), "utf8");
    expect(JSON.parse(read("share/phux-desktop/release.json"))).toEqual({
      identifier: "dev.phux.desktop",
      brandProfile: desktopBrand().id,
      version: "1.2.3",
      sha: "a".repeat(40),
      channel: "stable",
      target: "x86_64-unknown-linux-gnu",
    });
    expect(read("share/applications/dev.phux.desktop.desktop")).toContain("Exec=phux-desktop");
    expect(read("share/icons/hicolor/scalable/apps/dev.phux.desktop.svg")).toContain("<svg");
    expect(read("share/phux-desktop/THIRD-PARTY-NOTICES.txt")).toBe("test notices");
    expect(read("share/phux-desktop/PaperMono-OFL.txt")).toContain("SIL OPEN FONT LICENSE");
    expect(read("share/phux-desktop/Inter-OFL.txt")).toContain("SIL OPEN FONT LICENSE");
    expect(() => createLinuxBundle({ ...options, target: "aarch64-unknown-linux-gnu" })).toThrow(
      "ELF",
    );
    writeFileSync(binary, "not an ELF");
    expect(() => createLinuxBundle(options)).toThrow("ELF");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
