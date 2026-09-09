#!/usr/bin/env python3
"""Routing policy regressions; run without a compiler or third-party packages."""

from pathlib import Path
import json
import os
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SURFACES = ("phux", "cockpit", "web", "web_engine", "integrations", "native")
ALL = set(SURFACES)


def classify(paths):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/ci/classify-changes.sh")],
        input="\n".join(paths), text=True, capture_output=True, check=True,
    )
    return dict(line.split("=", 1) for line in result.stdout.splitlines())


class RoutingTests(unittest.TestCase):
    def test_surface_policy(self):
        cases = [
            ([], ALL),
            (["new-product/source.xyz"], ALL),
            (["docs/RELEASING.md"], set()),
            (["clients/cockpit/README.md"], set()),
            (["skills/phux/SKILL.md"], {"phux", "cockpit"}),
            (["clients/cockpit/src/main.zig"], {"cockpit"}),
            (["clients/phux-web/src/lib.rs"], {"web"}),
            (["clients/phux-vt-web/src/lib.rs"], {"web"}),
            (["scripts/ci/web-browser.py"], {"web"}),
            (["clients/phux-vt-web/vendor/ghostty-vt.wasm"], {"web", "web_engine"}),
            (["scripts/build-vt-wasm.sh"], {"web", "web_engine"}),
            (["integrations/pi/src/index.ts"], {"integrations"}),
            (["integrations/claude/skills/phux/SKILL.md"], {"integrations"}),
            ([".claude-plugin/marketplace.json"], {"integrations"}),
            (["crates/phux-server/src/lib.rs"], {"phux", "cockpit", "web"}),
            (["crates/phux-protocol/Cargo.toml"], {"phux", "cockpit", "web", "native"}),
            (["crates/phux-protocol/src/lib.rs"], {"phux", "cockpit", "web"}),
            (["crates/phux-client-core/src/lib.rs"], {"phux", "cockpit", "web"}),
            (["crates/phux-perf/src/lib.rs"], {"phux", "cockpit", "web"}),
            (["crates/phux-client-ffi/src/lib.rs"], {"phux", "cockpit", "web"}),
            (["crates/phux/build.rs"], {"phux", "cockpit", "native"}),
            (["Cargo.toml"], {"phux", "cockpit", "web", "native"}),
            (["Cargo.lock"], {"phux", "cockpit", "web", "native"}),
            ([".cargo/config.toml"], {"phux", "cockpit", "web", "native"}),
            ([".config/zig-toolchain.json"], {"phux", "cockpit", "web", "web_engine", "native"}),
            (["scripts/install-zig.sh"], {"phux", "cockpit", "web", "web_engine", "native"}),
            (["scripts/setup-rust.sh"], {"phux", "cockpit", "web", "native"}),
            ([".github/workflows/release.yml"], set()),
            ([".github/workflows/native-setup.yml"], set()),
            ([".github/actions/setup-rust-lane/action.yml"], set()),
            (["scripts/ci/classify-changes.sh"], set()),
            (["scripts/ci/validation_receipt.py"], set()),
            (["scripts/ci/wait_validation.py"], set()),
            (["scripts/ci/cockpit_artifacts.py"], {"cockpit"}),
            (["clients/cockpit/src/main.zig", "integrations/pi/src/index.ts"], {"cockpit", "integrations"}),
            (["docs/SETUP.md", "clients/phux-web/src/lib.rs"], {"web"}),
        ]
        for paths, wanted in cases:
            with self.subTest(paths=paths):
                outputs = classify(paths)
                self.assertEqual(
                    {key for key in SURFACES if outputs.get(key + "_needed") == "true"},
                    wanted,
                )
                for key in SURFACES:
                    self.assertIn(outputs.get(key + "_needed"), ("true", "false"))

    def test_all_coordinator_crates(self):
        # Conservative whole root-crate closure covers the bundled CLI and FFI.
        for manifest in (ROOT / "crates").glob("*/Cargo.toml"):
            with self.subTest(crate=manifest.parent.name):
                outputs = classify([str(manifest.parent.relative_to(ROOT) / "src/lib.rs")])
                self.assertEqual(outputs["phux_needed"], "true")
                self.assertEqual(outputs["cockpit_needed"], "true")
                self.assertEqual(outputs["native_needed"], "false")

    def test_cheap_flags(self):
        for paths, docs, workflows in [
            (["docs/SETUP.md", "clients/cockpit/README.md"], "true", "false"),
            (["skills/phux/SKILL.md"], "false", "false"),
            ([".github/workflows/ci.yml", ".github/workflows/release.yml"], "false", "true"),
            ([".github/workflows/ci.yml", "crates/phux/src/main.rs"], "false", "false"),
            ([], "false", "false"),
        ]:
            with self.subTest(paths=paths):
                output = classify(paths)
                self.assertEqual(output["docs_only"], docs)
                self.assertEqual(output["workflow_only"], workflows)

    def test_browser_native_server_dependency_closure(self):
        manifests = {path.parent.name: tomllib.loads(path.read_text())
                     for path in (ROOT / "crates").glob("*/Cargo.toml")}
        visited = set()
        pending = ["phux-server", "phux-client-core", "phux-protocol"]
        while pending:
            name = pending.pop()
            if name in visited:
                continue
            visited.add(name)
            manifest = manifests[name]
            tables = [manifest, *manifest.get("target", {}).values()]
            dependencies = {dependency for table in tables
                            for key in ("dependencies", "dev-dependencies", "build-dependencies")
                            for dependency in table.get(key, {})}
            pending.extend(dependencies & manifests.keys() - visited)
            with self.subTest(crate=name):
                self.assertEqual(classify([f"crates/{name}/src/lib.rs"])["web_needed"], "true")


class EventTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="phux-routing-")
        cls.repo = Path(cls.temporary.name)
        cls.git("init", "--quiet")
        cls.git("config", "user.email", "routing@example.invalid")
        cls.git("config", "user.name", "Routing Test")
        cls.git("config", "core.hooksPath", "/dev/null")
        cls.git("commit", "--quiet", "--allow-empty", "-m", "base")
        cls.base = cls.git("rev-parse", "HEAD").strip()

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    @classmethod
    def git(cls, *args):
        return subprocess.check_output(["git", *args], cwd=cls.repo, text=True)

    def revision(self, paths):
        self.git("checkout", "--quiet", "--detach", self.base)
        for name in paths:
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", "fixture")
        return self.git("rev-parse", "HEAD").strip()

    def detect(self, name, event):
        # Store payload outside the fixture git tree, exactly like Actions.
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json") as payload:
            json.dump(event, payload)
            payload.flush()
            result = subprocess.run(
                ["python3", str(ROOT / "scripts/ci/detect-changes.py")],
                cwd=self.repo, env={**os.environ, "GITHUB_EVENT_NAME": name,
                                    "GITHUB_EVENT_PATH": payload.name},
                capture_output=True, text=True, check=True,
            )
        return dict(line.split("=", 1) for line in result.stdout.splitlines())

    def test_push_pr_and_merge_group_routes(self):
        for paths in [
            [], ["docs/SETUP.md"], ["integrations/pi/src/index.ts"],
            ["clients/cockpit/src/main.zig"], ["clients/phux-web/src/lib.rs"],
            ["crates/phux-server/src/lib.rs"], ["crates/phux-protocol/Cargo.toml"],
            ["Cargo.toml"], ["new-product/source.xyz"],
            [".github/workflows/release.yml"], ["skills/phux/SKILL.md"],
            ["clients/cockpit/src/main.zig", "clients/phux-web/src/lib.rs"],
        ]:
            head = self.revision(paths)
            events = {
                "push": {"before": self.base, "after": head},
                "pull_request": {"pull_request": {"base": {"sha": self.base}, "head": {"sha": head}}},
                "merge_group": {"merge_group": {"base_sha": self.base, "head_sha": head}},
            }
            for name, payload in events.items():
                with self.subTest(event=name, paths=paths):
                    self.assertEqual(self.detect(name, payload), classify(paths))

    def test_pr_uses_merge_base_not_unrelated_base_branch_changes(self):
        head = self.revision(["docs/SETUP.md"])
        advanced_base = self.revision(["new-product/unrelated.xyz"])
        payload = {"pull_request": {"base": {"sha": advanced_base}, "head": {"sha": head}}}
        self.assertEqual(self.detect("pull_request", payload), classify(["docs/SETUP.md"]))

    def test_unknown_history_and_explicit_events_fail_closed(self):
        for name, payload in [
            ("workflow_dispatch", {}), ("schedule", {}), ("new_event", {}),
            ("push", {"before": "0" * 40, "after": self.base}),
            ("push", {"before": "f" * 40, "after": self.base}),
            ("push", {"before": "--help", "after": self.base}),
            ("pull_request", {}), ("merge_group", {}), ("push", []),
        ]:
            with self.subTest(name=name, payload=payload):
                self.assertEqual(self.detect(name, payload), classify([]))

    def test_cross_surface_rename_checks_both_paths(self):
        before = self.revision(["clients/cockpit/fixture.txt"])
        target = self.repo / "clients/phux-web/fixture.txt"
        target.parent.mkdir(parents=True, exist_ok=True)
        self.git("mv", "clients/cockpit/fixture.txt", str(target))
        self.git("commit", "--quiet", "-m", "move")
        after = self.git("rev-parse", "HEAD").strip()
        self.assertEqual(
            self.detect("push", {"before": before, "after": after}),
            classify(["clients/cockpit/fixture.txt", "clients/phux-web/fixture.txt"]),
        )

    def test_unrepresentable_filename_fails_closed(self):
        head = self.revision(["docs/line\nbreak.md"])
        self.assertEqual(self.detect("push", {"before": self.base, "after": head}), classify([]))


class WorkflowTests(unittest.TestCase):
    def test_cheap_changes_do_not_allocate_rust_setup(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        cheap_jobs = workflow.split("  workflow-gate:", 1)[1].split("  check:", 1)[0]
        self.assertNotIn("nix develop", cheap_jobs)
        self.assertNotIn("setup-rust-lane", cheap_jobs)
        self.assertIn("nix shell --inputs-from .", cheap_jobs)
        self.assertIn("if: needs.changes.outputs.integrations_needed == 'true'", cheap_jobs)
        self.assertEqual(workflow.count("if: needs.changes.outputs.phux_needed == 'true'"), 2)

    def test_shared_detection_has_no_outer_path_filter(self):
        for name in ("ci", "native-setup", "cockpit-ci", "web-check"):
            workflow = (ROOT / f".github/workflows/{name}.yml").read_text()
            triggers = workflow.split("concurrency:")[0]
            with self.subTest(workflow=name):
                self.assertNotIn("paths:", triggers)
                self.assertIn("merge_group:", triggers)
                self.assertIn("fetch-depth: 0", workflow)
                self.assertIn("uses: ./.github/actions/classify-changes", workflow)
                self.assertIn("cancel-in-progress: true", workflow)

    def test_aggregate_rejects_failed_and_cancelled_new_lanes(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        javascript = workflow.split("node -e '", 1)[1].split("\n          '\n", 1)[0]
        baseline = {name: {"result": "skipped"} for name in ("check", "test", "integrations")}
        baseline.update({name: {"result": "success"} for name in ("changes", "workflow-gate")})
        for job, result, expected in [
            ("check", "skipped", 0), ("integrations", "success", 0),
            ("integrations", "failure", 1), ("test", "cancelled", 1),
            ("workflow-gate", "failure", 1), ("workflow-gate", "skipped", 1),
            ("changes", "skipped", 1), ("changes", "failure", 1),
        ]:
            with self.subTest(job=job, result=result):
                needs = {**baseline, job: {"result": result}}
                actual = subprocess.run(
                    ["node", "-e", javascript], capture_output=True, text=True,
                    env={**os.environ, "RESULTS": json.dumps(needs)},
                )
                self.assertEqual(actual.returncode, expected, actual.stderr)


if __name__ == "__main__":
    unittest.main()
