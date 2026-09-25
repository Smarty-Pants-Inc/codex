import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class RequiredNextestTests(unittest.TestCase):
    def test_each_requirement_must_match_selected_nonignored_test(self):
        script = Path(__file__).with_name("check_required_nextest_tests.py")
        for ignored, match_status, requirements, expected_success in [
            (False, "matches", ["^pkg::new_case$", "^pkg::family::"], True),
            (False, "matches", ["^pkg::new_case$", "^pkg::absent$"], False),
            (False, "matches", ["^other_pkg::new_case$"], False),
            (True, "matches", ["^pkg::new_case$"], False),
            (False, "mismatch", ["^pkg::new_case$"], False),
        ]:
            with self.subTest(
                ignored=ignored, status=match_status, required=requirements
            ):
                inventory = {
                    "rust-suites": {
                        "pkg": {
                            "package-name": "pkg",
                            "status": "listed",
                            "testcases": {
                                name: {
                                    "ignored": ignored,
                                    "filter-match": {"status": match_status},
                                }
                                for name in ["new_case", "family::companion"]
                            },
                        }
                    }
                }
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / "inventory.json"
                    path.write_text(json.dumps(inventory))
                    result = subprocess.run(
                        [sys.executable, str(script), str(path), *requirements],
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                self.assertEqual(
                    result.returncode == 0, expected_success, result.stderr
                )
                if expected_success:
                    self.assertEqual(
                        json.loads(result.stdout.splitlines()[-1]),
                        {"compiled_selected": 2, "runtime_executed": False},
                    )


if __name__ == "__main__":
    unittest.main()
