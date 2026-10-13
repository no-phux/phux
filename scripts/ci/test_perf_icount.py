#!/usr/bin/env python3
"""Comparator and routing checks for the instruction-count guard."""

from pathlib import Path
import json
import subprocess
import unittest

import perf_icount

ROOT = Path(__file__).resolve().parents[2]


def summary(name, ident, instructions):
    return {
        "function_name": name,
        "id": ident,
        "profiles": [
            {
                "tool": "Callgrind",
                "summaries": {
                    "total": {
                        "summary": {
                            "Callgrind": {
                                "Ir": {
                                    "diffs": None,
                                    "metrics": {"Left": {"Int": instructions}},
                                }
                            }
                        }
                    }
                },
            }
        ],
    }


class ExtractTests(unittest.TestCase):
    def test_left_and_both_keep_the_new_count(self):
        left = summary("vt_apply", "apply", 1000)
        self.assertEqual(perf_icount.instruction_count(left), 1000)
        both = summary("vt_apply", "apply", 1)
        both["profiles"][0]["summaries"]["total"]["summary"]["Callgrind"]["Ir"]["metrics"] = {
            "Both": [{"Int": 1100}, {"Int": 1000}]
        }
        self.assertEqual(perf_icount.instruction_count(both), 1100)

    def test_load_names_scenarios_by_function_and_id(self):
        text = "\n".join([
            json.dumps(summary("vt_apply", "apply", 10)),
            json.dumps(summary("frame_encode", "encode", 20)),
            "not a summary",
        ])
        # The third line is JSON-invalid and must fail, not be skipped.
        with self.assertRaises(ValueError):
            perf_icount.load_measurements(text + "\n{")
        found = perf_icount.load_measurements(text.replace("not a summary", ""))
        self.assertEqual(found, {"vt_apply/apply": 10, "frame_encode/encode": 20})


class LimitTests(unittest.TestCase):
    def test_exactly_five_percent_passes_and_one_more_fails(self):
        self.assertFalse(perf_icount.over_limit(105, 100, 5))
        self.assertTrue(perf_icount.over_limit(106, 100, 5))
        self.assertFalse(perf_icount.over_limit(90, 100, 5))

    def test_compare_reports_missing_and_extra(self):
        failures = perf_icount.compare(
            {"a": 100, "extra": 1},
            {"increase_limit_percent": 5, "scenarios": {"a": 100, "missing": 1}},
        )
        self.assertEqual(len(failures), 2)
        self.assertTrue(any("missing" in line for line in failures))
        self.assertTrue(any("extra" in line for line in failures))


class TrailerTests(unittest.TestCase):
    def test_trailer_is_a_whole_line(self):
        self.assertTrue(perf_icount.trailer_present("subject\n\nPerf-Baseline: update\n"))
        self.assertFalse(perf_icount.trailer_present("Perf-Baseline: update now"))
        self.assertFalse(perf_icount.trailer_present("see Perf-Baseline: update\n"))


class RouteTests(unittest.TestCase):
    def classify(self, paths):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/ci/classify-changes.sh")],
            input="\n".join(paths),
            text=True,
            capture_output=True,
            check=True,
        )
        return dict(line.split("=", 1) for line in result.stdout.splitlines())

    def test_hot_paths_select_the_job_and_docs_do_not(self):
        cases = {
            "": "true",
            "crates/phux-server/src/lib.rs": "true",
            "crates/phux-protocol/src/lib.rs": "true",
            "crates/phux-client-core/src/lib.rs": "true",
            "crates/phux-core/src/org.rs": "true",
            "benchmarks/icount-baseline.json": "true",
            "just/perf.just": "true",
            ".github/workflows/ci.yml": "true",
            "docs/performance.md": "false",
            "crates/phux-tui/src/lib.rs": "false",
            "scripts/bench/perf-nightly.sh": "false",
        }
        for path, want in cases.items():
            with self.subTest(path=path):
                files = [] if path == "" else [path]
                self.assertEqual(self.classify(files)["perf_icount_needed"], want)


if __name__ == "__main__":
    unittest.main()
