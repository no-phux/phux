import { expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const script = resolve(import.meta.dir, "../../../../scripts/ci/sign-macos-desktop.sh");
const configuration = {
  APPLE_DEVELOPER_ID_P12: "fixture-not-a-certificate",
  APPLE_DEVELOPER_ID_PASSWORD: "fixture-password",
  APPLE_NOTARY_KEY_P8: "fixture-not-a-private-key",
  APPLE_NOTARY_KEY_ID: "1234ABCDEF",
  APPLE_NOTARY_ISSUER_ID: "12345678-1234-1234-1234-123456789abc",
};

test("stable signing fails closed for every missing Apple credential", () => {
  for (const missing of Object.keys(configuration)) {
    const env: Record<string, string> = { ...configuration, [missing]: "" };
    const result = Bun.spawnSync(["/bin/bash", script, "--check-config"], { env });
    expect(result.exitCode).toBe(1);
    expect(result.stderr.toString()).toContain(`Missing ${missing}`);
  }
});

// Command fixtures exercise orchestration and cleanup, NOT Apple's trust service.
// No fixture invokes security, codesign, or notarytool on the real computer.
const commands: Record<string, string> = {
  security: `if [[ "$1" == list-keychains ]]; then
    if [[ "$*" != *' -s'* ]]; then
      if [[ "$TEST_FAIL" != empty ]]; then
        printf '    "/fixture/login.keychain-db"\n    "/fixture/extra keys.keychain-db"\n'
      fi
    elif [[ "\${5:-}" == */release.keychain-db ]]; then
      if [[ "$TEST_FAIL" == empty ]]; then
        test "$#" = 5
      else
        test "$#" = 7 && test "$6" = /fixture/login.keychain-db && test "$7" = '/fixture/extra keys.keychain-db'
      fi
      touch "$TEST_SEARCH_LIST"
    else
      if [[ "$TEST_FAIL" == empty ]]; then
        test "$#" = 4
      else
        test "$#" = 6 && test "$5" = /fixture/login.keychain-db && test "$6" = '/fixture/extra keys.keychain-db'
      fi
      rm -f "$TEST_SEARCH_LIST"
    fi
  elif [[ "$1" == find-identity ]]; then
    echo '  1) 1234567890123456789012345678901234567890 "Developer ID Application: Fixture"'
  fi`,
  codesign: `if [[ "$*" == *' --sign '* ]]; then
    test -f "$TEST_SEARCH_LIST" || { echo 'no identity found' >&2; exit 1; }
  fi`,
  spctl: ":",
  uuidgen: "echo fixture-keychain-password",
  ditto: 'touch "${@: -1}"',
  bun: 'touch "${@: -1}"',
  xcrun: `if [[ "$1" == notarytool ]]; then
    status=Accepted
    if [[ "$TEST_FAIL" == app && "$3" == */submission.zip ]]; then status=Invalid; fi
    if [[ "$TEST_FAIL" == dmg && "$3" == *.dmg ]]; then status=Invalid; fi
    printf '{"status":"%s","id":"fixture-submission"}\\n' "$status"
  fi`,
};

test.each(["none", "app", "dmg", "empty"])(
  "signing transaction handles %s outcome and removes temporary keys",
  (failure) => {
    const root = mkdtempSync(join(tmpdir(), "phux-sign-test-"));
    try {
      const bin = join(root, "bin");
      const app = join(root, "Phux.app");
      const log = join(root, "commands.log");
      const archive = join(root, "output/Phux.zip");
      mkdirSync(bin);
      mkdirSync(join(app, "Contents/MacOS"), { recursive: true });
      writeFileSync(join(app, "Contents/MacOS/phux"), "fixture CLI", { mode: 0o755 });
      writeFileSync(join(app, "Contents/Info.plist"), "fixture app");
      for (const [name, body] of Object.entries(commands)) {
        writeFileSync(
          join(bin, name),
          `#!/bin/bash\nset -eu\nprintf '%s %s\\n' '${name}' "$*" >> "$TEST_LOG"\n${body}\n`,
          { mode: 0o755 },
        );
      }
      const result = Bun.spawnSync(["/bin/bash", script, app, archive], {
        env: {
          ...configuration,
          APPLE_DEVELOPER_ID_P12: Buffer.from("fixture-p12").toString("base64"),
          APPLE_NOTARY_KEY_P8: Buffer.from("fixture-p8").toString("base64"),
          PATH: `${bin}:${process.env.PATH ?? "/usr/bin:/bin"}`,
          TMPDIR: root,
          TEST_LOG: log,
          TEST_SEARCH_LIST: join(root, "search-list"),
          TEST_FAIL: failure,
        },
      });
      expect(result.exitCode).toBe(failure === "none" || failure === "empty" ? 0 : 1);
      const calls = readFileSync(log, "utf8");
      expect(calls).toContain(`--options runtime --timestamp ${app}/Contents/MacOS/phux`);
      expect(calls.indexOf(`--timestamp ${app}/Contents/MacOS/phux`)).toBeLessThan(
        calls.indexOf(`--timestamp ${app}\n`),
      );
      expect(calls).toContain("security delete-keychain");
      expect(calls).toContain(
        failure === "empty"
          ? "security list-keychains -d user -s\n"
          : "security list-keychains -d user -s /fixture/login.keychain-db /fixture/extra keys.keychain-db",
      );
      expect(calls.indexOf("security list-keychains -d user -s")).toBeLessThan(
        calls.indexOf("codesign --force"),
      );
      expect(existsSync(join(root, "search-list"))).toBe(false);
      expect(readdirSync(root).some((name) => name.startsWith("phux-apple-sign."))).toBe(false);
      if (failure === "app") {
        expect(existsSync(archive)).toBe(false);
        expect(calls).not.toContain("stapler staple");
      } else {
        expect(existsSync(archive)).toBe(true);
        expect(calls.indexOf(`stapler staple ${app}`)).toBeLessThan(calls.indexOf(`bun `));
        expect(existsSync(join(root, "output/Phux.dmg"))).toBe(true);
      }
      if (failure === "dmg") expect(calls).not.toContain("spctl --assess --type open");
      expect(JSON.parse(readFileSync(join(root, "output/Phux-app-notary.json"), "utf8"))).toEqual({
        status: failure === "app" ? "Invalid" : "Accepted",
        id: "fixture-submission",
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test("configuration preflight validates IDs without importing keys or submitting to Apple", () => {
  const result = Bun.spawnSync(["/bin/bash", script, "--check-config"], { env: configuration });
  expect(result.exitCode).toBe(0);
  for (const invalid of [
    { APPLE_NOTARY_KEY_ID: "not-an-id" },
    { APPLE_NOTARY_ISSUER_ID: "not-an-issuer" },
  ]) {
    const result = Bun.spawnSync(["/bin/bash", script, "--check-config"], {
      env: { ...configuration, ...invalid },
    });
    expect(result.exitCode).toBe(1);
    expect(result.stderr.toString()).toContain("Invalid Apple notary");
  }
});
