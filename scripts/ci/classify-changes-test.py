#!/usr/bin/env python3
"""Routing policy regressions; run without a compiler or third-party packages."""

from pathlib import Path
import json
import os
import re
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
SURFACES = ("phux", "web", "web_engine", "integrations", "native")
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
            (["docs/spec/proto.md"], {"phux"}),
            (["docs/spec/wire-schema.json"], {"phux"}),
            (["docs/reference/cli.md"], {"phux"}),
            (["docs/assets/pi-live-fleet.cast"], {"phux"}),
            (["docs/consumers/agents.md"], {"phux"}),
            (["CHANGELOG.md"], set()),
            ([".agents/skills/using-phux/SKILL.md"], {"phux"}),
            ([".agents/skills/using-phux-mcp/SKILL.md"], {"phux"}),
            (["clients/desktop/native/src/main.rs"], ALL),
            (["clients/phux-web/src/lib.rs"], {"web"}),
            (["clients/phux-vt-web/src/lib.rs"], {"web"}),
            (["scripts/ci/web-browser.py"], {"web"}),
            (["scripts/ci/web_agent_fixture.py"], {"web"}),
            (["clients/phux-vt-web/vendor/ghostty-vt.wasm"], {"web", "web_engine"}),
            (["scripts/build-vt-wasm.sh"], {"web", "web_engine"}),
            (["integrations/pi/src/index.ts"], {"integrations"}),
            (["scripts/ci/agent-integrations.sh"], {"integrations"}),
            (["integrations/claude/skills/phux/SKILL.md"], {"integrations"}),
            ([".claude-plugin/marketplace.json"], {"integrations"}),
            (["crates/phux-server/src/lib.rs"], {"phux", "web"}),
            (["crates/phux-protocol/Cargo.toml"], {"phux", "web", "native"}),
            (["crates/phux-protocol/src/lib.rs"], {"phux", "web"}),
            (["crates/phux-client-core/src/lib.rs"], {"phux", "web"}),
            (["crates/phux-perf/src/lib.rs"], {"phux", "web"}),
            (["crates/phux-client-ffi/src/lib.rs"], {"phux", "web"}),
            (["crates/phux/build.rs"], {"phux", "web", "native"}),
            (["Cargo.toml"], {"phux", "web", "native"}),
            (["Cargo.lock"], {"phux", "web", "native"}),
            ([".cargo/config.toml"], {"phux", "web", "native"}),
            ([".config/zig-toolchain.json"], {"phux", "web", "web_engine", "native"}),
            (["scripts/install-zig.sh"], {"native", "web_engine"}),
            (["scripts/lib/dev-toolchain.sh"], {"native", "web_engine"}),
            (["scripts/setup-rust.sh"], {"native"}),
            (["scripts/doctor.sh"], {"native"}),
            (["scripts/test-dev-setup.sh"], {"native"}),
            (["scripts/native-smoke.sh"], {"native"}),
            (["just/setup.just"], {"native"}),
            (["just/gates.just"], {"phux"}),
            (["just/test.just"], {"phux"}),
            (["just/build.just"], {"phux"}),
            (["justfile"], set()),
            (["just/perf.just"], set()),
            (["just/release.just"], set()),
            (["just/mutation.just"], set()),
            (["release-please-config.json"], set()),
            (["just/unknown.just"], ALL),
            (["docs/SETUP.md", "scripts/doctor.sh"], {"native"}),
            (["docs/SETUP.md", "justfile"], set()),
            (["scripts/check-e2e-lanes.sh"], set()),
            (["scripts/check-docs.sh"], set()),
            (["scripts/check-install-surface.sh"], set()),
            (["scripts/check-product-skills-export.sh"], set()),
            (["scripts/export-product-skills.sh"], set()),
            (["scripts/product-skills"], set()),
            (["scripts/skills-package/README.md"], set()),
            (["scripts/skills-package/NOTICE"], set()),
            (["scripts/build-ffi-xcframework.sh"], set()),
            (["scripts/build-mobile-ffi-android.sh"], set()),
            (["scripts/ci/setup-android-ndk.sh"], set()),
            ([".github/workflows/ffi-xcframework.yml"], set()),
            ([".github/workflows/ffi-android.yml"], set()),
            ([".github/workflows/release.yml"], set()),
            ([".github/workflows/native-setup.yml"], set()),
            ([".github/actions/setup-rust-lane/action.yml"], set()),
            (["scripts/ci/classify-changes.sh"], set()),
            (["scripts/ci/release_metadata.py"], set()),
            (["scripts/ci/test_release_metadata.py"], set()),
            (["scripts/ci/validation_receipt.py"], set()),
            (["scripts/ci/publish_plan.py"], set()),
            (["scripts/ci/dispatch_integration_publishes.py"], set()),
            (["scripts/ci/extract_changelog_section.py"], set()),
            (["scripts/ci/setup-linux-release-userspace.sh"], set()),
            (["scripts/ci/test_runner_policy.py"], set()),
            (["scripts/ci/mbx-export-failed-lane.sh"], set()),
            (["scripts/check-release-orchestration.mjs"], set()),
            (["scripts/ci/test_release_drift.py"], set()),
            (["scripts/ci/test_next_channel.py"], set()),
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
        for manifest in (ROOT / "crates").glob("*/Cargo.toml"):
            with self.subTest(crate=manifest.parent.name):
                outputs = classify([str(manifest.parent.relative_to(ROOT) / "src/lib.rs")])
                self.assertEqual(outputs["phux_needed"], "true")
                self.assertEqual(outputs["native_needed"], "false")
                self.assertNotIn("cockpit_needed", outputs)

    def test_server_test_file_does_not_rebuild_browser_or_the_workspace(self):
        test_file = classify(["crates/phux-server/tests/hub_relay_federation.rs"])
        self.assertEqual(test_file["web_needed"], "false")
        self.assertEqual(test_file["unit_mode"], "narrow")
        self.assertEqual(test_file["unit_targets"], "phux-server:hub_relay_federation")
        self.assertEqual(test_file["e2e_needed"], "false")
        library = classify(["crates/phux-server/src/lib.rs"])
        self.assertEqual(library["unit_mode"], "rdeps")

    def test_cheap_flags(self):
        for paths, docs, workflows in [
            (["docs/SETUP.md", "clients/desktop/README.md"], "true", "false"),
            (["docs/adr/0001-x.md", "README.md", "docs/INSTALL.md"], "true", "false"),
            (["docs/SETUP.md", "docs/spec/proto.md"], "false", "false"),
            (["docs/reference/cli.md"], "false", "false"),
            (["CHANGELOG.md"], "false", "false"),
            ([".agents/skills/using-phux/SKILL.md"], "false", "false"),
            ([".github/workflows/ci.yml", ".github/workflows/release.yml"], "false", "true"),
            ([".github/workflows/website-dispatch.yml"], "false", "true"),
            ([".github/workflows/ffi-xcframework.yml"], "false", "true"),
            ([".github/workflows/ci.yml", "crates/phux/src/main.rs"], "false", "false"),
            ([], "false", "false"),
        ]:
            with self.subTest(paths=paths):
                output = classify(paths)
                self.assertEqual(output["docs_only"], docs)
                self.assertEqual(output["workflow_only"], workflows)

    def test_docs_read_by_crates_leave_the_docs_only_path(self):
        # A quoted docs/ path in Rust source is a test fixture or an embedded
        # input; editing it must run the lane that reads it.
        literal = re.compile(r'"(?:\.\./)*(docs/[^"\s{}]+)"')
        read = set()
        for source in (ROOT / "crates").rglob("*.rs"):
            for match in literal.finditer(source.read_text(encoding="utf-8")):
                if (ROOT / match.group(1)).exists():
                    read.add(match.group(1).rstrip("/"))
        self.assertTrue(read)
        for path in sorted(read):
            probe = path if (ROOT / path).is_file() else path + "/probe"
            with self.subTest(path=path):
                outputs = classify([probe])
                self.assertEqual(outputs["docs_only"], "false")
                self.assertEqual(outputs["phux_needed"], "true")

    def test_affected_crate_filterset_is_fail_safe(self):
        # Empty or unmappable -> full pool. Crate-only diffs emit exact rdeps.
        cases = [
            ([], ""),
            (["docs/SETUP.md"], ""),
            (["Cargo.toml"], ""),
            (["justfile"], ""),
            ([".agents/skills/using-phux/SKILL.md"], ""),
            ([".github/workflows/ci.yml"], ""),
            (["new-product/source.xyz"], ""),
            (["crates/phux-protocol/src/lib.rs"], "rdeps(=phux-protocol)"),
            (
                ["crates/phux-tui/src/lib.rs", "docs/SETUP.md"],
                "rdeps(=phux-tui)",
            ),
            (
                ["crates/phux-protocol/src/lib.rs", "crates/phux-tui/src/lib.rs"],
                "rdeps(=phux-protocol) + rdeps(=phux-tui)",
            ),
            (["crates/phux-protocol/src/lib.rs", "Cargo.lock"], ""),
        ]
        for paths, wanted in cases:
            with self.subTest(paths=paths):
                self.assertEqual(classify(paths)["test_filterset"], wanted)

    def test_every_workspace_crate_maps_to_its_package_name(self):
        manifests = {
            path.parent.name: tomllib.loads(path.read_text())["package"]["name"]
            for path in (ROOT / "crates").glob("*/Cargo.toml")
        }
        self.assertTrue(manifests)
        for directory, name in manifests.items():
            with self.subTest(crate=directory):
                self.assertEqual(
                    classify([f"crates/{directory}/src/lib.rs"])["test_filterset"],
                    f"rdeps(={name})",
                )

    def test_browser_native_server_dependency_closure(self):
        manifests = {path.parent.name: tomllib.loads(path.read_text())
                     for path in (ROOT / "crates").glob("*/Cargo.toml")}
        visited = set()
        pending = ["phux", "phux-server", "phux-client-core", "phux-protocol"]
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
            ["clients/desktop/native/src/main.rs"], ["clients/phux-web/src/lib.rs"],
            ["crates/phux-server/src/lib.rs"], ["crates/phux-protocol/Cargo.toml"],
            ["Cargo.toml"], ["new-product/source.xyz"],
            [".github/workflows/release.yml"], [".agents/skills/using-phux/SKILL.md"],
            ["integrations/pi/src/index.ts", "clients/phux-web/src/lib.rs"],
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
        before = self.revision(["integrations/pi/fixture.txt"])
        target = self.repo / "clients/phux-web/fixture.txt"
        target.parent.mkdir(parents=True, exist_ok=True)
        self.git("mv", "integrations/pi/fixture.txt", str(target))
        self.git("commit", "--quiet", "-m", "move")
        after = self.git("rev-parse", "HEAD").strip()
        self.assertEqual(
            self.detect("push", {"before": before, "after": after}),
            classify(["integrations/pi/fixture.txt", "clients/phux-web/fixture.txt"]),
        )

    def test_unrepresentable_filename_fails_closed(self):
        head = self.revision(["docs/line\nbreak.md"])
        self.assertEqual(self.detect("push", {"before": self.base, "after": head}), classify([]))


class WorkflowTests(unittest.TestCase):
    def test_root_justfile_is_imports_and_default_only(self):
        # Product recipes in the root justfile would classify as cheap and
        # silently skip the lane they belong to. Keep them in just/*.just.
        text = (ROOT / "justfile").read_text()
        recipes = []
        for line in text.splitlines():
            if line.startswith("import ") or line.startswith("#") or not line.strip():
                continue
            if line.startswith("[") or line.startswith(" ") or line.startswith("\t"):
                continue
            if line.startswith("default:"):
                continue
            if ":" in line and not line.lstrip().startswith("#"):
                recipes.append(line)
        self.assertEqual(recipes, [], msg=f"product recipes belong in just/*.just: {recipes}")

    def test_cheap_changes_do_not_allocate_rust_setup(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        cheap_jobs = workflow.split("  workflow-gate:", 1)[1].split("  check:", 1)[0]
        self.assertNotIn("nix develop", cheap_jobs)
        self.assertNotIn("setup-rust-lane", cheap_jobs)
        self.assertIn("nix shell --inputs-from .", cheap_jobs)
        self.assertIn("if: needs.changes.outputs.integrations_needed == 'true'", cheap_jobs)
        # check and the four test-profile lanes.
        self.assertEqual(workflow.count("if: needs.changes.outputs.phux_needed == 'true'"), 5)

    def test_pr_unit_lane_uses_classifier_rdeps_filterset(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        recipe = "\n".join(
            path.read_text()
            for path in [ROOT / "justfile", *sorted((ROOT / "just").glob("*.just"))]
        )
        action = (ROOT / ".github/actions/classify-changes/action.yml").read_text()
        self.assertIn("test_filterset:", workflow)
        self.assertIn("PHUX_NEXTEST_FILTERSET:", workflow)
        self.assertIn("github.event_name != 'push'", workflow)
        self.assertIn("needs.changes.outputs.test_filterset", workflow)
        self.assertIn("PHUX_NEXTEST_FILTERSET", recipe)
        self.assertIn("cargo nextest run --workspace", recipe)
        self.assertIn('PHUX_UNIT_MODE:-workspace}" == "narrow"', recipe)
        self.assertIn("test_filterset:", action)

    def test_shared_detection_has_no_outer_path_filter(self):
        for name in ("ci", "native-setup", "web-check"):
            workflow = (ROOT / f".github/workflows/{name}.yml").read_text()
            triggers = workflow.split("concurrency:")[0]
            with self.subTest(workflow=name):
                self.assertNotIn("paths:", triggers)
                self.assertIn("merge_group:", triggers)
                self.assertIn("fetch-depth: 0", workflow)
                self.assertIn("uses: ./.github/actions/classify-changes", workflow)
                self.assertIn("cancel-in-progress: true", workflow)

    def test_archived_cockpit_has_no_workflows_or_artifact_outputs(self):
        self.assertEqual(list((ROOT / ".github/workflows").glob("cockpit-*.yml")), [])
        outputs = classify([])
        for key in ("cockpit_needed", "ffi_needed", "cli_needed", "zig_needed", "shipping_needed"):
            self.assertNotIn(key, outputs)

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
