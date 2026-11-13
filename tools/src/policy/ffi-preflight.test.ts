// Execute the shared guard with real jq/checksum/file operations and an inert
// gh function. No fixture can dispatch a build, read credentials, or publish.
import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { capture } from "../lib/process.ts";
import { repoPath } from "../lib/repo.ts";
import { str, table } from "../lib/value.ts";
import { job, run, steps, uses, withInput, workflow } from "./workflows.ts";

const WORKFLOWS = ["ffi-xcframework.yml", "ffi-android.yml"];
const PIN = "a".repeat(40);
const OTHER = "b".repeat(40);
const TAG = "v1.2.3";
const MOCK_GH = `
evidence() {
    command "$@"
    local status=$?
    [[ "$status" -eq 0 ]] || return "$status"
    if [[ "$1" == "$FAIL_COMMAND" && "$*" == *"$FAIL_MATCH"* ]]; then
        return 1
    fi
}
jq() { evidence jq "$@"; }
awk() { evidence awk "$@"; }
sha256sum() { evidence sha256sum "$@"; }
mktemp() { evidence mktemp "$@"; }
mock_gh() {
    case "$1 $2" in
        "run list")
            test "$FAIL_API" != list || return 1
            printf '%s' "$FIXTURE_RUNS" ;;
        "release view") printf '%s' "$FIXTURE_RELEASE" ;;
        "release download")
            local dir="" pattern
            local -a patterns=()
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    --dir) dir="$2"; shift 2 ;;
                    --pattern) patterns+=("$2"); shift 2 ;;
                    *) shift ;;
                esac
            done
            for pattern in "\${patterns[@]}"; do
                cp "$RELEASE_FILES/$pattern" "$dir/$pattern" || return 1
            done ;;
        api*)
            case "$2" in
                */commits/*) printf '%s' "$FIXTURE_PIN" ;;
                */artifacts*)
                    test "$FAIL_API" != artifacts || return 1
                    printf '%s' "$FIXTURE_ARTIFACTS" ;;
                */jobs*)
                    test "$FAIL_API" != jobs || return 1
                    printf '%s' "$FIXTURE_JOBS" | jq -r "$4" ;;
                *) echo "unexpected API: $*" >&2; return 2 ;;
            esac ;;
        *) echo "unexpected gh: $*" >&2; return 2 ;;
    esac
}
gh() {
    mock_gh "$@" || return $?
    [[ "$FAIL_COMMAND" != gh || "$*" != *"$FAIL_MATCH"* ]]
}
source "$PREFLIGHT_SCRIPT"
`;

function decision(name: string) {
  return steps(job(workflow(name), "preflight")).find((step) => step["id"] === "decide") ?? {};
}

function artifacts(name: string, suffix = "", expired = false): string {
  const bare = str(table(decision(name)["env"])["BARE_ARTIFACTS"]) ?? "";
  return JSON.stringify({
    artifacts: bare.split(" ").map((value) => ({ name: value + suffix, expired })),
  });
}

function fixture(
  name: string,
  overrides: Record<string, string> = {},
  files: Record<string, string> = {},
): boolean {
  const dir = mkdtempSync(join(tmpdir(), "phux-ffi-preflight-"));
  mkdirSync(join(dir, "release"));
  for (const [file, content] of Object.entries(files)) {
    writeFileSync(join(dir, "release", file), content);
  }
  const stepEnv = Object.fromEntries(
    Object.entries(table(decision(name)["env"])).map(([key, value]) => [key, String(value)]),
  );
  try {
    const output = capture(["bash", "-c", MOCK_GH], {
      env: {
        ...process.env,
        ...stepEnv,
        GH_TOKEN: "",
        GH_REPO: "no-phux/phux",
        RELEASE_TAG: "",
        COMMIT_INPUT: "",
        GITHUB_SHA: PIN,
        GITHUB_RUN_ID: "2",
        GITHUB_OUTPUT: "/dev/stdout",
        GITHUB_STEP_SUMMARY: "/dev/null",
        RUNNER_TEMP: dir,
        TMPDIR: dir,
        RELEASE_FILES: join(dir, "release"),
        PREFLIGHT_SCRIPT: repoPath("scripts/ci/ffi-build-preflight.sh"),
        FAIL_API: "",
        FAIL_COMMAND: "",
        FAIL_MATCH: "",
        FIXTURE_PIN: PIN,
        FIXTURE_RELEASE: "{}",
        FIXTURE_RUNS: JSON.stringify([
          {
            databaseId: 1,
            headSha: PIN,
            event: "workflow_dispatch",
            url: "https://github.com/no-phux/phux/actions/runs/1",
            createdAt: "2026-11-13",
          },
        ]),
        FIXTURE_ARTIFACTS: artifacts(name),
        FIXTURE_JOBS: JSON.stringify({
          jobs: [{ steps: [{ name: "Validate release tag", conclusion: "skipped" }] }],
        }),
        ...overrides,
      },
      timeoutMs: 10_000,
    });
    expect(output).toMatch(/skip=(true|false)/);
    return output.includes("skip=true");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test.each(WORKFLOWS)("%s executes the guard from the harness SHA, not the built pin", (name) => {
  const preflight = steps(job(workflow(name), "preflight"));
  const checkout = preflight.find((step) => uses(step).startsWith("actions/checkout@")) ?? {};
  expect(withInput(checkout, "ref")).toBe("${{ github.sha }}");
  expect(withInput(checkout, "persist-credentials")).toBe(false);
  expect(run(decision(name))).toBe("bash scripts/ci/ffi-build-preflight.sh");
  expect(table(decision(name)["env"])["BARE_ARTIFACTS"]).toBe(
    name === "ffi-xcframework.yml"
      ? "PhuxFFI-xcframework PhuxMobileFFI-xcframework"
      : "PhuxMobileFFI-android",
  );
  const marker = steps(job(workflow(name), "build")).filter(
    (step) => step["name"] === "Validate release tag",
  );
  expect(marker).toHaveLength(1);
  expect(marker[0]?.["if"]).toBe("env.RELEASE_TAG != ''");
});

test.each(WORKFLOWS)("%s reuses a positively untagged same-head build", (name) => {
  expect(fixture(name)).toBe(true);
});

for (const conclusion of ["absent", "success", "failure"]) {
  test.each(WORKFLOWS)(`%s rejects ${conclusion} tag-validation proof`, (name) => {
    const marker = { name: "Validate release tag", conclusion };
    const entries = conclusion === "absent" ? [] : [marker];
    expect(fixture(name, { FIXTURE_JOBS: JSON.stringify({ jobs: [{ steps: entries }] }) })).toBe(
      false,
    );
  });
}

for (const api of ["list", "artifacts", "jobs"]) {
  test.each(WORKFLOWS)(`%s builds when ${api} evidence is unreadable`, (name) => {
    expect(fixture(name, { FAIL_API: api })).toBe(false);
  });
}

test.each(WORKFLOWS)("%s rejects missing or expired artifacts", (name) => {
  expect(fixture(name, { FIXTURE_ARTIFACTS: '{"artifacts":[]}' })).toBe(false);
  expect(fixture(name, { FIXTURE_ARTIFACTS: artifacts(name, "", true) })).toBe(false);
});

test.each(WORKFLOWS)("%s rejects artifact JSON followed by malformed JSON", (name) => {
  expect(fixture(name, { FIXTURE_ARTIFACTS: artifacts(name) + "\n{" })).toBe(false);
});

test.each(WORKFLOWS)("%s rejects artifact jq output when jq exits nonzero", (name) => {
  expect(fixture(name, { FAIL_COMMAND: "jq", FAIL_MATCH: ".artifacts[]" })).toBe(false);
});

for (const [command, match] of [
  ["gh", "run list"],
  ["gh", "/artifacts"],
  ["gh", "/jobs"],
  ["jq", "add"],
]) {
  test.each(WORKFLOWS)(`%s rejects valid run evidence from failing ${command} ${match}`, (name) => {
    expect(fixture(name, { FAIL_COMMAND: command ?? "", FAIL_MATCH: match ?? "" })).toBe(false);
  });
}

test.each(WORKFLOWS)("%s rejects wrong-head bare artifacts and duplicate tag evidence", (name) => {
  expect(
    fixture(name, {
      FIXTURE_RUNS: JSON.stringify([
        {
          databaseId: 1,
          headSha: OTHER,
          event: "workflow_dispatch",
          url: "",
          createdAt: "2026-11-13",
        },
      ]),
    }),
  ).toBe(false);
  const marker = { name: "Validate release tag", conclusion: "skipped" };
  expect(
    fixture(name, {
      FIXTURE_JOBS: JSON.stringify({ jobs: [{ steps: [marker, marker] }] }),
    }),
  ).toBe(false);
});

test("xcframework reuse requires both C and mobile artifacts", () => {
  expect(
    fixture("ffi-xcframework.yml", {
      FIXTURE_ARTIFACTS: '{"artifacts":[{"name":"PhuxFFI-xcframework","expired":false}]}',
    }),
  ).toBe(false);
});

test.each(WORKFLOWS)("%s requires the full pinned SHA suffix on a later-head dispatch", (name) => {
  expect(fixture(name, { COMMIT_INPUT: OTHER })).toBe(false);
  expect(
    fixture(name, {
      COMMIT_INPUT: OTHER,
      FIXTURE_ARTIFACTS: artifacts(name, `-${OTHER}`),
    }),
  ).toBe(true);
});

function releaseFiles(name: string): Record<string, string> {
  const specs = str(table(decision(name)["env"])["RELEASE_SPECS"]) ?? "";
  const files: Record<string, string> = {};
  for (const spec of specs.trim().split("\n")) {
    const [prefix, zipSuffix, provSuffix, line] = spec.split("|");
    const zip = `${prefix}${TAG}${zipSuffix}`;
    files[zip] = "fixture archive bytes";
    files[`${zip}.sha256`] = `${createHash("sha256").update(files[zip]).digest("hex")}  ${zip}\n`;
    files[`${prefix}${TAG}${provSuffix}`] = `${line}${line?.endsWith("=") ? "" : " "}${PIN}\n`;
  }
  return files;
}

for (const corruption of [
  "none",
  "missing",
  "missing-last",
  "checksum",
  "checksum-fields",
  "provenance",
  "provenance-duplicate",
  "provenance-conflicting",
  "target",
]) {
  test.each(WORKFLOWS)(`%s release replay checks ${corruption}`, (name) => {
    const files = releaseFiles(name);
    const names = Object.keys(files);
    const zip = Object.keys(files).find((file) => file.endsWith(".zip")) ?? "";
    const prov = Object.keys(files).find((file) => file.endsWith(".provenance")) ?? "";
    if (corruption === "missing") delete files[zip];
    if (corruption === "missing-last") {
      const last = names.filter((file) => file.endsWith(".zip")).at(-1) ?? "";
      delete files[last];
    }
    if (corruption === "checksum") files[zip] = "wrong archive bytes";
    if (corruption === "checksum-fields")
      files[`${zip}.sha256`] = files[`${zip}.sha256`]?.trim() + " extra\n";
    if (corruption === "provenance") files[prov] = `phux_rev=${OTHER}\n`;
    if (corruption.startsWith("provenance-")) {
      for (const file of names.filter((file) => file.endsWith(".provenance"))) {
        const original = files[file] ?? "";
        files[file] =
          original +
          (corruption === "provenance-duplicate" ? original : original.replace(PIN, OTHER));
      }
    }
    const view = {
      isDraft: false,
      targetCommitish: corruption === "target" ? OTHER : PIN,
      assets: names.map((file) => ({ name: file })),
    };
    expect(fixture(name, { RELEASE_TAG: TAG, FIXTURE_RELEASE: JSON.stringify(view) }, files)).toBe(
      corruption === "none",
    );
  });
}

for (const [command, match] of [
  ["jq", ".isDraft"],
  ["jq", ".targetCommitish"],
  ["jq", ".assets[].name"],
  ["awk", "n++"],
  ["awk", "tolower"],
  ["awk", "print $2"],
  ["sha256sum", ""],
  ["awk", "print $1"],
  ["awk", "index($0"],
  ["mktemp", "-d"],
  ["gh", "release view"],
  ["gh", "release download"],
  ["gh", "/commits/"],
]) {
  test.each(WORKFLOWS)(`%s rejects release evidence from failing ${command} ${match}`, (name) => {
    const files = releaseFiles(name);
    const view = {
      isDraft: false,
      targetCommitish: PIN,
      assets: Object.keys(files).map((file) => ({ name: file })),
    };
    expect(
      fixture(
        name,
        {
          RELEASE_TAG: TAG,
          FIXTURE_RELEASE: JSON.stringify(view),
          FAIL_COMMAND: command ?? "",
          FAIL_MATCH: match ?? "",
        },
        files,
      ),
    ).toBe(false);
  });
}

test.each(WORKFLOWS)("%s rejects release JSON followed by malformed JSON", (name) => {
  const files = releaseFiles(name);
  const view = {
    isDraft: false,
    targetCommitish: PIN,
    assets: Object.keys(files).map((file) => ({ name: file })),
  };
  expect(
    fixture(name, { RELEASE_TAG: TAG, FIXTURE_RELEASE: JSON.stringify(view) + "\n{" }, files),
  ).toBe(false);
});

for (const duplicate of [PIN, OTHER]) {
  for (const name of WORKFLOWS) {
    for (const prov of Object.keys(releaseFiles(name)).filter((file) =>
      file.endsWith(".provenance"),
    )) {
      test(`${name} rejects an extra ${duplicate === PIN ? "identical" : "conflicting"} field in ${prov}`, () => {
        const files = releaseFiles(name);
        const original = files[prov] ?? "";
        files[prov] = original + original.replace(PIN, duplicate);
        const view = {
          isDraft: false,
          targetCommitish: PIN,
          assets: Object.keys(files).map((file) => ({ name: file })),
        };
        expect(
          fixture(name, { RELEASE_TAG: TAG, FIXTURE_RELEASE: JSON.stringify(view) }, files),
        ).toBe(false);
      });
    }
  }
}
