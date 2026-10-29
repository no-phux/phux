import { expect, test } from "bun:test";
import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { capture } from "../lib/process.ts";
import { repoPath, ROOT } from "../lib/repo.ts";
import { action, job, jobs, run, steps, uses, withInput, workflow } from "../policy/workflows.ts";
import { table } from "../lib/value.ts";

function dryRun(...args: string[]): string {
  const result = Bun.spawnSync(
    ["just", "--dry-run", "--color", "never", "desktop-framework-check", ...args],
    { cwd: ROOT },
  );
  expect(result.exitCode).toBe(0);
  return result.stdout.toString() + result.stderr.toString();
}

test("PR qualification builds and packages debug, while the public default stays release", () => {
  const debug = dryRun("debug");
  expect(debug).toContain("--debug");
  expect(debug).toContain("native/target/debug/phux-desktop");
  expect(debug).not.toContain("--release");
  const release = dryRun();
  expect(release).toContain("--release");
  expect(release).toContain("native/target/release/phux-desktop");
});

test.each([false, true])(
  "shared desktop tests use selected source capabilities (bootstrap=%s)",
  (bootstrap) => {
    const root = mkdtempSync(join(tmpdir(), "phux-desktop-qualification-"));
    try {
      const bin = join(root, "bin");
      mkdirSync(bin);
      const source = join(root, "source");
      mkdirSync(source);
      const observed = join(root, "observed");
      // Historical sources have neither the wrapper nor the new bootstrap recipe.
      writeFileSync(
        join(source, "justfile"),
        "desktop-test:\n    @printf 'tests\\n' >> \"$QUALIFICATION_LOG\"\ndesktop-renderer-test:\n    @printf 'renderer\\n' >> \"$QUALIFICATION_LOG\"\ndesktop-perf-budget:\n    @printf 'perf\\n' >> \"$QUALIFICATION_LOG\"\n",
      );
      if (bootstrap) {
        appendFileSync(
          join(source, "justfile"),
          "desktop-bootstrap-test:\n    @printf 'bootstrap\\n' >> \"$QUALIFICATION_LOG\"\n",
        );
      }
      const extra = bootstrap ? "bootstrap\n" : "";
      for (const [platform, expected] of [
        ["Linux", `tests\n${extra}renderer\n`],
        ["Darwin", `tests\n${extra}perf\n`],
      ] as const) {
        writeFileSync(join(bin, "uname"), `#!/usr/bin/env bash\nprintf '%s\\n' '${platform}'\n`, {
          mode: 0o755,
        });
        writeFileSync(observed, "");
        capture(
          [
            "just",
            "--justfile",
            repoPath("justfile"),
            "--working-directory",
            source,
            "desktop-tests",
          ],
          {
            cwd: root,
            env: {
              ...process.env,
              PATH: `${bin}:${process.env["PATH"] ?? ""}`,
              DISPLAY: ":fixture",
              QUALIFICATION_LOG: observed,
            },
          },
        );
        expect(readFileSync(observed, "utf8")).toBe(expected);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test("macOS provisioning is shared but still reads toolchain pins from the selected source", () => {
  const setup = action("setup-macos-desktop");
  expect(steps(setup)[0]?.["working-directory"]).toBe("${{ inputs.source }}");
  const ciSetup = Object.values(jobs(workflow("ci.yml")))
    .flatMap(steps)
    .find((step) => uses(step) === "./.github/actions/setup-macos-desktop");
  expect(ciSetup).toBeDefined();
  const releaseSteps = Object.values(jobs(workflow("desktop-release.yml"))).flatMap(steps);
  const releaseSetup = releaseSteps.find(
    (step) => uses(step) === "./harness/.github/actions/setup-macos-desktop",
  );
  expect(withInput(releaseSetup ?? {}, "source")).toBe("source");
  expect(
    releaseSteps.some((step) =>
      run(step).includes('--working-directory "$GITHUB_WORKSPACE/source" desktop-tests'),
    ),
  ).toBe(true);
  expect(releaseSteps.filter((step) => run(step) === "just desktop-package").length).toBe(2);
});

test("only stable macOS receives Apple credentials, before upload and after packaging", () => {
  const release = workflow("desktop-release.yml");
  const macosSteps = steps(job(release, "macos"));
  const signed = macosSteps.findIndex((step) => String(step["name"]).startsWith("Sign, notarize"));
  expect(signed).toBeGreaterThan(
    macosSteps.findIndex((step) => run(step) === "just desktop-package"),
  );
  expect(signed).toBeLessThan(
    macosSteps.findIndex((step) => run(step).includes("qualify-macos-desktop.sh")),
  );
  expect(signed).toBeLessThan(
    macosSteps.findIndex((step) => run(step).includes("gh release upload")),
  );
  const signStep = macosSteps[signed] ?? {};
  expect(signStep["if"]).toBe("inputs.channel == 'stable'");
  for (const secret of [
    "APPLE_DEVELOPER_ID_P12",
    "APPLE_DEVELOPER_ID_PASSWORD",
    "APPLE_NOTARY_KEY_P8",
    "APPLE_NOTARY_KEY_ID",
    "APPLE_NOTARY_ISSUER_ID",
  ]) {
    expect(table(signStep["env"])[secret]).toBe(`\${{ secrets.${secret} }}`);
    expect(table(job(workflow("release.yml"), "desktop")["secrets"])[secret]).toBe(
      `\${{ secrets.${secret} }}`,
    );
    expect(table(job(workflow("next.yml"), "desktop")["secrets"])[secret]).toBeUndefined();
  }
  expect(
    macosSteps.some((step) => run(step).includes("sign-macos-desktop.sh --check-config")),
  ).toBe(true);
  const qualification = readFileSync(repoPath("scripts/ci/qualify-macos-desktop.sh"), "utf8");
  expect(qualification).toContain("hdiutil attach -readonly -nobrowse");
  expect(qualification).toContain("spctl --assess --type execute");
});

test("Windows builds native MSVC and signs only after a same-source WSL GUI journey", () => {
  const windows = workflow("windows-desktop.yml");
  expect(job(windows, "windows")["runs-on"]).toBe("windows-2025");
  expect(job(windows, "windows")["needs"]).toBeUndefined();
  const build = steps(job(windows, "windows"));
  expect(build.some((step) => run(step).includes("--target x86_64-pc-windows-msvc"))).toBe(true);
  expect(build.some((step) => run(step).includes("rust-toolchain.toml"))).toBe(true);
  expect(build.some((step) => run(step).includes("install-zig-windows.ps1"))).toBe(true);
  expect(build.some((step) => withInput(step, "name") === "desktop-build-windows-x64")).toBe(true);
  expect(build.some((step) => withInput(step, "name") === "desktop-windows-x64")).toBe(false);
  const qualified = steps(job(windows, "qualify"));
  const smoke = qualified.findIndex((step) => run(step).includes("smoke-windows.ps1"));
  const product = qualified.findIndex((step) => withInput(step, "name") === "desktop-windows-x64");
  expect(smoke).toBeGreaterThan(0);
  expect(product).toBeGreaterThan(smoke);
  expect(qualified[product]?.["if"]).toBeUndefined();
  expect(
    steps(job(windows, "server")).some(
      (step) => run(step) === "cargo build --locked --release -p phux",
    ),
  ).toBe(true);
  const installer = readFileSync(repoPath("scripts/install-desktop-windows.ps1"), "utf8");
  const keys =
    readFileSync(repoPath("crates/phux-client-runtime/src/release/signature.rs"), "utf8").match(
      /"RW[A-Za-z0-9+/=]+"/g,
    ) ?? [];
  expect(keys).toHaveLength(2);
  for (const key of keys) expect(installer).toContain(key.slice(1, -1));
  const smokeScript = readFileSync(repoPath("clients/desktop/scripts/smoke-windows.ps1"), "utf8");
  for (const proof of [
    "SendInput",
    "CopyFromScreen",
    "Get-ConnectionEpoch",
    "PHUXSURVIVALAFTERCRASH",
  ])
    expect(smokeScript).toContain(proof);
  expect(
    readFileSync(repoPath("clients/desktop/scripts/smoke-windows-resize.ps1"), "utf8"),
  ).toContain("stty -F /proc/$shellPid/fd/0 size");
});

test("Windows policy filters cannot silently pass zero tests and diagnostics never publish products", () => {
  const windows = workflow("windows-desktop.yml");
  const qualify = steps(job(windows, "qualify"));
  const policy = qualify.map(run).find((script) => script.includes("$cases =")) ?? "";
  expect(policy).toContain("--list --format terse");
  expect(policy).toContain('$listed.Contains("${case}: test")');
  expect(policy).toContain("--exact");
  expect(policy).toContain("test result: ok\\. 1 passed; 0 failed; 0 ignored;");
  expect(policy).toContain(
    "connection::home::probe_tests::topology_before_seed_cannot_select_the_recent_session_as_home",
  );
  expect(policy).toContain(
    "connection::home::probe_tests::an_old_refusal_cannot_release_a_reconnected_probe_before_its_seed",
  );
  expect(policy).toContain(
    "connection::home::probe_tests::a_not_ready_probe_rearms_the_wake_for_its_seed",
  );
  const predicates =
    steps(job(windows, "windows"))
      .map(run)
      .find((script) => script.includes("presentation::tests")) ?? "";
  expect(predicates).toContain("-p gpui-fast-windows");
  expect(predicates).toContain("test result: ok\\. 3 passed; 0 failed; 0 ignored;");
  const ordering =
    steps(job(windows, "windows"))
      .map(run)
      .find((script) => script.includes("fast::input_order::tests")) ?? "";
  expect(ordering).toContain("test result: ok\\. 5 passed; 0 failed; 0 ignored;");
  expect(ordering).toContain("$exitCode -ne 0");
  expect(qualify.map(run).join("\n")).not.toContain("-DiagnosticBuildSha");

  const diagnostic = workflow("windows-desktop-diagnostic.yml");
  expect(diagnostic.permissions).toEqual({ contents: "read", actions: "read" });
  const diagnosticSteps = steps(job(diagnostic, "diagnose"));
  expect(diagnosticSteps.map(run).join("\n")).toContain("-DiagnosticBuildSha");
  expect(diagnosticSteps.some((step) => withInput(step, "name") === "desktop-windows-x64")).toBe(
    false,
  );
  expect(diagnosticSteps.map(run).join("\n")).not.toContain("gh release");
  const smoke = readFileSync(repoPath("clients/desktop/scripts/smoke-windows.ps1"), "utf8");
  expect(smoke).toContain("$expectedVersion = $release.version");
  expect(smoke).toContain("$env:PHUX_DIAGNOSTIC -cne 'true'");
  const clipboard = readFileSync(
    repoPath("clients/desktop/scripts/smoke-windows-clipboard.ps1"),
    "utf8",
  );
  expect(clipboard).toContain("@('snapshot', '--unwrap', $session)");
  expect(clipboard).toContain("Get-FileHash -Algorithm SHA256");
  expect(clipboard).toContain("[Windows.Forms.Clipboard]::SetFileDropList");
  expect(clipboard).toContain("[Windows.Forms.Clipboard]::SetImage");
  const connected = smoke.indexOf("(Get-ConnectionEpoch) -gt $epoch");
  const injected = smoke.indexOf(
    "@('send-keys', $session, (Get-MarkerCommand 'RECONNECTREADY'), 'Enter')",
  );
  const ready = smoke.indexOf("Assert-Rendered 'reconnect-ready' 'PHUXRECONNECTREADY'");
  const typed = smoke.indexOf("Send-Shell (Get-MarkerCommand 'RECONNECTED')");
  expect(connected).toBeGreaterThan(0);
  expect(injected).toBeGreaterThan(connected);
  expect(ready).toBeGreaterThan(injected);
  expect(typed).toBeGreaterThan(ready);
});

test("desktop release workflows preserve legacy targets and require Windows at the cutover", () => {
  const release = workflow("desktop-release.yml");
  const select = steps(job(release, "capabilities")).find((step) => step["id"] === "targets");
  expect(select).toBeDefined();
  const root = mkdtempSync(join(tmpdir(), "phux-desktop-capabilities-"));
  try {
    for (const [channel, tag, expected] of [
      ["stable", "desktop-v0.49.0", "windows=false\nsidecars=4\n"],
      ["stable", "desktop-v0.50.0", "windows=true\nsidecars=5\n"],
      ["stable", "desktop-v1.0.0", "windows=true\nsidecars=5\n"],
      ["next", "next", "windows=true\nsidecars=5\n"],
    ] as const) {
      const output = join(root, `${channel}-${tag}`);
      const result = Bun.spawnSync(["bash", "-e", "-c", run(select ?? {})], {
        cwd: ROOT,
        env: {
          ...process.env,
          GITHUB_OUTPUT: output,
          RELEASE_CHANNEL: channel,
          RELEASE_TAG: tag,
          RELEASE_SHA: "a".repeat(40),
        },
      });
      expect(result.exitCode).toBe(0);
      expect(readFileSync(output, "utf8")).toBe(expected);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
  expect(job(release, "windows")["if"]).toBe("needs.capabilities.outputs.windows == 'true'");
  expect(job(release, "sign")["if"]).toContain("needs.windows.result == 'success'");
  expect(job(release, "sign")["if"]).toContain("needs.capabilities.outputs.windows == 'false'");
  expect(job(release, "sign")["if"]).toContain("needs.windows.result == 'skipped'");
  expect(steps(job(release, "sign")).map(run).join("\n")).toContain(
    "bash scripts/ci/publish-desktop-release.sh",
  );
  expect(readFileSync(repoPath("scripts/ci/publish-desktop-release.sh"), "utf8")).toContain(
    '"$EXPECTED_DESKTOP_SIDECARS"',
  );
});
