#!/usr/bin/env python3
"""Exercise the real just recipes without installing or executing mutation tools."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class RecipeArguments(unittest.TestCase):
    def test_language_recipes_preserve_argument_boundaries(self):
        scratch = "/private/tmp/opencode" if Path("/private/tmp/opencode").is_dir() else None
        with tempfile.TemporaryDirectory(dir=scratch) as temporary:
            directory = Path(temporary)
            output = directory / "argv.json"
            stub = directory / "bash"
            stub.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "from pathlib import Path\n"
                "Path(os.environ['MUTATION_ARGV_LOG']).write_text(json.dumps(sys.argv[1:]))\n"
            )
            stub.chmod(0o755)
            env = dict(os.environ, PATH=str(directory) + os.pathsep + os.environ["PATH"],
                       MUTATION_ARGV_LOG=str(output))
            cases = {
                "rust": ["--re", "validate_ratio|match guard.*with true", "--list",
                         "--file", "crates/**/*.rs", "--output", "reports with spaces"],
                "zig": ["--scope", "module with spaces.zig", "--list",
                        "--out", "reports; with $shell characters"],
            }
            for language, args in cases.items():
                with self.subTest(language=language):
                    subprocess.run(["just", "mutation-" + language, *args], cwd=ROOT,
                                   env=env, check=True)
                    self.assertEqual(json.loads(output.read_text()),
                                     ["scripts/mutation/" + language + ".sh", *args])


if __name__ == "__main__":
    unittest.main()
