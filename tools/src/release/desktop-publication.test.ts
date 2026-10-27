import { expect, test } from "bun:test";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";

test.each([
  [false, "valid"],
  [true, "valid"],
  [true, "missing"],
  [true, "tampered"],
] as const)("stable desktop publication (Windows=%s, bytes=%s)", (windows, variant) => {
  const root = mkdtempSync(join(tmpdir(), "desktop-publication-"));
  try {
    for (const path of ["bin", "scripts/ci", "desktop-sidecar", "draft"]) {
      mkdirSync(join(root, path), { recursive: true });
    }
    copyFileSync(repoPath("scripts/ci/publish-desktop-release.sh"), join(root, "publish.sh"));
    // No network, production key, or real GitHub publication: record the exact
    // sequence and serve immutable local draft checksum bytes.
    writeFileSync(
      join(root, "bin/gh"),
      `#!/usr/bin/env bash
set -euo pipefail
echo "$2" >> "$CASE_ROOT/calls"
case "$2" in
  download)
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --pattern) name="$2"; shift 2 ;;
        --dir) destination="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    mkdir -p "$destination"
    cp "$CASE_ROOT/draft/$name" "$destination/$name" ;;
  view) printf '\\n' ;;
  upload) ;;
  edit) [[ "$*" == *'--draft=false --latest=false'* ]] ;;
  *) exit 2 ;;
esac
`,
      { mode: 0o755 },
    );
    writeFileSync(
      join(root, "scripts/ci/sign-release.sh"),
      `#!/usr/bin/env bash
set -euo pipefail
echo sign >> "$CASE_ROOT/calls"
for path in "$@"; do printf 'fixture signature\\n' > "$path.minisig"; done
`,
    );
    const version = windows ? "0.50.0" : "0.49.0";
    const suffixes = [
      "macos-arm64.zip",
      "macos-arm64.dmg",
      "linux-x64.tar.gz",
      "linux-arm64.tar.gz",
    ];
    if (windows && variant !== "missing") suffixes.push("windows-x64.zip");
    for (const suffix of suffixes) {
      const name = `phux-desktop-${version}-${suffix}`;
      const checksum = `${"a".repeat(64)}  ${name}\n`;
      writeFileSync(join(root, "desktop-sidecar", `${name}.sha256`), checksum);
      writeFileSync(
        join(root, "draft", `${name}.sha256`),
        variant === "tampered" ? "different bytes\n" : checksum,
      );
    }
    const result = Bun.spawnSync(["bash", "publish.sh"], {
      cwd: root,
      env: {
        ...process.env,
        PATH: `${join(root, "bin")}:${process.env["PATH"] ?? ""}`,
        CASE_ROOT: root,
        RUNNER_TEMP: root,
        RELEASE_TAG: `desktop-v${version}`,
        EXPECTED_DESKTOP_SIDECARS: windows ? "5" : "4",
        WINDOWS_DESKTOP: String(windows),
      },
    });
    const callsPath = join(root, "calls");
    if (variant === "valid") {
      expect(result.exitCode).toBe(0);
      const calls = readFileSync(callsPath, "utf8");
      expect(calls).toContain("sign\n");
      expect(calls).toContain("edit\n");
      expect(calls.indexOf("sign\n")).toBeLessThan(calls.indexOf("edit\n"));
      expect(
        readFileSync(join(root, "desktop-release-notes"), "utf8").includes("Windows x64:"),
      ).toBe(windows);
    } else {
      expect(result.exitCode).not.toBe(0);
      expect(!existsSync(callsPath) || !readFileSync(callsPath, "utf8").includes("sign\n")).toBe(
        true,
      );
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
