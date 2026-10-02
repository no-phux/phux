#!/usr/bin/env python3
"""Linear publication must establish a version before updating its stage.

The pinned Linear action's sync command creates/updates the supplied version;
update cannot create it. These contracts run without credentials or API writes.
"""

from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/linear-release.yml").read_text()
PUBLISH = (ROOT / ".github/workflows/publish.yml").read_text()
STEPS = re.split(r"(?=^      - )", WORKFLOW, flags=re.M)[1:]
BASE_CONDITION = "success() && steps.keys.outputs.skip != 'true'"


def field(block, key, indent=10):
    match = re.search(rf"^{' ' * indent}{re.escape(key)}: (.+)$", block, re.M)
    return match.group(1) if match else None


def action(component, command):
    matches = [
        block for block in STEPS
        if "uses: linear/linear-release-action@" in block
        and field(block, "command") == command
        and f"steps.meta.outputs.component == '{component}'" in field(block, "if", 8)
    ]
    if len(matches) != 1:
        raise AssertionError(f"expected exactly one {component} {command} step")
    return matches[0]


class LinearReleaseWorkflowTests(unittest.TestCase):
    def test_released_entry_establishes_record_without_building_dispatch(self):
        for component in ("phux", "cockpit"):
            with self.subTest(component=component):
                sync = action(component, "sync")
                update = action(component, "update")
                self.assertEqual(field(sync, "if", 8), f"{BASE_CONDITION} && steps.meta.outputs.component == '{component}'")
                self.assertEqual(field(update, "if", 8), f"{BASE_CONDITION} && inputs.stage == 'released' && steps.meta.outputs.component == '{component}'")
                self.assertLess(STEPS.index(sync), STEPS.index(update))
                self.assertEqual(field(update, "stage"), "Released")

    def test_sync_and_update_reuse_identical_version_and_pipeline_on_retry(self):
        for component, secret in (("phux", "LINEAR_RELEASE_ACCESS_KEY"), ("cockpit", "LINEAR_COCKPIT_RELEASE_ACCESS_KEY")):
            sync, update = action(component, "sync"), action(component, "update")
            for step in (sync, update):
                self.assertEqual(field(step, "version"), "${{ inputs.tag }}")
                self.assertEqual(field(step, "access_key"), "${{ secrets." + secret + " }}")
            self.assertEqual(field(sync, "name"), "${{ inputs.tag }}")
            self.assertIsNone(field(sync, "stage"), "a late building sync must not downgrade Released")
        self.assertEqual(field(action("cockpit", "sync"), "include_paths"), "clients/cockpit/**")

    def test_notes_are_present_for_both_entry_points_before_sync(self):
        notes = next(block for block in STEPS if "- name: Write Linear release notes" in block)
        self.assertEqual(field(notes, "if", 8), BASE_CONDITION)
        for component in ("phux", "cockpit"):
            sync = action(component, "sync")
            self.assertLess(STEPS.index(notes), STEPS.index(sync))
            self.assertEqual(field(sync, "release_notes"), "release-notes.md")

    def test_old_tag_recovery_keeps_current_helper_and_tagged_changelog(self):
        copy = WORKFLOW.index('cp scripts/ci/extract_changelog_section.py "$RUNNER_TEMP/extract-changelog-section.py"')
        checkout = WORKFLOW.index('git checkout --quiet "refs/tags/${TAG}"')
        extract = WORKFLOW.index('python3 "$RUNNER_TEMP/extract-changelog-section.py"')
        self.assertLess(copy, checkout)
        self.assertLess(checkout, extract)
        self.assertIn('--tag "$TAG" --changelog "$CHANGELOG" --output release-notes.md', WORKFLOW)

    def test_same_tag_stages_are_serialized_without_cancellation(self):
        self.assertIn("group: mini-v1-linear-release-${{ inputs.tag }}\n", WORKFLOW)
        self.assertIn("cancel-in-progress: false", WORKFLOW)

    def test_released_reports_remain_gated_on_successful_publication(self):
        for component in ("phux", "cockpit"):
            match = re.search(rf"^  linear-{component}:\n(.*?)(?=^  [a-z]|\Z)", PUBLISH, re.M | re.S)
            self.assertIsNotNone(match)
            block = match.group(1)
            self.assertIn(f"needs: [plan, {component}]", block)
            self.assertIn(f"if: needs.{component}.result == 'success'", block)
            self.assertIn("stage: released", block)


if __name__ == "__main__":
    unittest.main()
