// The static half of the install surface: the installers phux.sh serves, how
// clients discover releases, and the tests that run an installer.
// install/test.sh runs the installer tests themselves.
import { expect, test } from "bun:test";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { Glob } from "bun";
import { repoPath, ROOT } from "../lib/repo.ts";

const read = (path: string): string => readFileSync(repoPath(path), "utf8");

/** A file, or every file under a directory. */
function files(path: string): string[] {
  if (!statSync(repoPath(path)).isDirectory()) return [path];
  return readdirSync(repoPath(path), { recursive: true, encoding: "utf8" })
    .map((child) => join(path, child))
    .filter((child) => statSync(repoPath(child)).isFile())
    .sort();
}

// no-phux/website serves these verbatim and refuses one without a POSIX shebang.
function shebangFailures(installers: Record<string, string>): string[] {
  return Object.entries(installers).flatMap(([path, text]) =>
    text.split("\n")[0] === "#!/bin/sh" ? [] : [`${path} must start with #!/bin/sh`],
  );
}

// Clients discover releases from the channel manifest only; the GitHub API's
// unauthenticated budget is 60 requests an hour. The one API use is
// API_RELEASES: re-fetching an already-named asset that releases/download
// failed to serve.
const RELEASE_LIST = /api\.github\.com|releases\/latest/;
const API_RELEASES =
  /^(pub const API_RELEASES: &str = "|API_RELEASES=)https:\/\/api\.github\.com\/repos\/no-phux\/phux\/releases("|$)/;

function releaseListFailures(client: string, texts: readonly string[]): string[] {
  const lines = texts.flatMap((text) => text.split("\n"));
  const offending = lines.some((line) => RELEASE_LIST.test(line) && !API_RELEASES.test(line));
  return offending ? [`${client} must read the channel manifest, not the GitHub release list`] : [];
}

// install.sh appends to the invoking shell's profile; a test that runs it
// against the real home leaves PATH lines behind (phux-gh85y).
function homeFailures(tests: Record<string, string>): string[] {
  return Object.entries(tests).flatMap(([path, text]) =>
    /install(-desktop|-rune)?\.sh|install\/src\//.test(text) && !/^export HOME="\$TMP/m.test(text)
      ? [`${path} runs an installer; export HOME="$TMP/..." first`]
      : [],
  );
}

const CLIENTS = [
  "scripts/install.sh",
  "scripts/install-windows.ps1",
  "crates/phux-client-runtime/src/release.rs",
  "crates/phux/src/commands/update",
  "clients/rune/src/updates.rs",
];

test("served installers start with a POSIX shebang", () => {
  const installers = ["scripts/install.sh", "scripts/install-desktop.sh"];
  expect(shebangFailures(Object.fromEntries(installers.map((path) => [path, read(path)])))).toEqual(
    [],
  );
});

test("release clients read the channel manifest, not the GitHub release list", () => {
  expect(CLIENTS.flatMap((client) => releaseListFailures(client, files(client).map(read)))).toEqual(
    [],
  );
});

test("installer tests run against a temporary HOME", () => {
  const tests = ["install", "packaging"]
    .flatMap((dir) => [...new Glob(`${dir}/test-*.sh`).scanSync(ROOT)])
    .sort();
  expect(homeFailures(Object.fromEntries(tests.map((path) => [path, read(path)])))).toEqual([]);
});

test("a bash installer, a release-list read, or a real-HOME installer test is refused", () => {
  expect(shebangFailures({ "a.sh": "#!/usr/bin/env bash\n", "b.sh": "#!/bin/sh\n" })).toEqual([
    "a.sh must start with #!/bin/sh",
  ]);
  const allowed = "API_RELEASES=https://api.github.com/repos/no-phux/phux/releases";
  expect(releaseListFailures("ok", [allowed])).toEqual([]);
  expect(releaseListFailures("rs", ['    let url = ".../releases/latest";'])).toHaveLength(1);
  expect(releaseListFailures("sh", [`  ${allowed}/latest`])).toHaveLength(1);
  expect(
    homeFailures({
      "scripts/test-a.sh": 'export HOME="$TMP/home"\nsh scripts/install.sh\n',
      "scripts/test-b.sh": "sh scripts/install-rune.sh\n",
      "scripts/test-c.sh": "echo unrelated\n",
    }),
  ).toEqual(['scripts/test-b.sh runs an installer; export HOME="$TMP/..." first']);
});
