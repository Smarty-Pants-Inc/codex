"""Exercise the real Linux CI loops with inert Cargo/just stand-ins."""

import ast
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


WORKFLOW = (Path(__file__).parents[1] / "workflows/smarty-ci.yml").read_text()


def workflow_run(name):
    step = WORKFLOW.split(f"      - name: {name}\n", 1)[1].split("      - ", 1)[0]
    return textwrap.dedent(step.split("        run: |\n", 1)[1])


@unittest.skipUnless(sys.platform == "linux", "CI uses Linux GNU time and Bash")
class PhaseReceiptTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "codex-rs").mkdir()
        self.output = self.root / "backend-evidence"
        tools = self.root / "tools"
        tools.mkdir()
        for tool in ("cargo", "just"):
            path = tools / tool
            path.write_text(
                f"#!{sys.executable}\n"
                + textwrap.dedent("""\
                import json
                import os
                from pathlib import Path
                import signal
                import sys

                args = sys.argv[1:]
                phase = 'discovery' if Path(sys.argv[0]).name == 'cargo' else args[0]
                project = args[args.index('-p') + 1]
                root = Path(os.environ['GITHUB_WORKSPACE'])
                with (root / 'calls.jsonl').open('a') as log:
                    log.write(json.dumps([phase, project, args, str(Path.cwd())]) + '\\n')
                if phase == 'test':
                    junit = root / 'codex-rs/target/nextest/local/junit.xml'
                    junit.parent.mkdir(parents=True, exist_ok=True)
                    junit.write_text(project)
                print('fixture stdout')
                print('fixture stderr', file=sys.stderr)
                if [phase, project] == os.environ['FIXTURE_FAILURE'].split():
                    code = int(os.environ['FIXTURE_EXIT'])
                    if code < 0:
                        os.kill(os.getpid(), signal.SIGTERM)
                    sys.exit(code)
                """)
            )
            path.chmod(0o755)
        time_format = next(
            line.split(": ", 1)[1]
            for line in WORKFLOW.splitlines()
            if line.strip().startswith("CI_TIME_FORMAT:")
        )
        self.env = {
            "PATH": f"{tools}:/usr/bin:/bin",
            "GITHUB_WORKSPACE": str(self.root),
            "PROJECTS": "fixture-first fixture-second",
            "CI_TIME_FORMAT": ast.literal_eval(time_format),
            "FIXTURE_FAILURE": "",
            "FIXTURE_EXIT": "19",
        }

    def run_step(self, name):
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c", workflow_run(name)],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def receipt(self, project, phase):
        output = self.output / project
        timing = json.loads((output / f"{phase}-time.json").read_text())
        self.assertEqual(
            set(timing),
            {"elapsed_seconds", "user_seconds", "system_seconds", "max_rss_kib"},
        )
        self.assertTrue(all(value >= 0 for value in timing.values()))
        return int((output / f"{phase}-exit.txt").read_text())

    def test_success_preserves_arguments_working_directories_and_output(self):
        result = self.run_step("Complete backend project tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_step("Scoped backend Clippy")
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = []
        for project in self.env["PROJECTS"].split():
            expected.extend(
                [
                    [
                        "discovery",
                        project,
                        [
                            "nextest",
                            "list",
                            "--locked",
                            "-p",
                            project,
                            "--message-format",
                            "json",
                        ],
                        str(self.root / "codex-rs"),
                    ],
                    [
                        "test",
                        project,
                        [
                            "test",
                            "--locked",
                            "-p",
                            project,
                            "--test-threads",
                            "2",
                            "--status-level",
                            "all",
                            "--final-status-level",
                            "all",
                        ],
                        str(self.root),
                    ],
                ]
            )
            for phase in ("discovery", "test", "clippy"):
                self.assertEqual(self.receipt(project, phase), 0)
            output = self.output / project
            self.assertEqual(
                (output / "discovered.json").read_text(), "fixture stdout\n"
            )
            self.assertEqual((output / "discovery.txt").read_text(), "fixture stderr\n")
            self.assertEqual((output / "junit.xml").read_text(), project)
        expected.extend(
            [
                "clippy",
                project,
                ["clippy", "--locked", "-p", project, "--", "-D", "warnings"],
                str(self.root),
            ]
            for project in self.env["PROJECTS"].split()
        )
        calls = [
            json.loads(line)
            for line in (self.root / "calls.jsonl").read_text().splitlines()
        ]
        self.assertEqual(calls, expected)

    def test_discovery_failure_does_not_reuse_prior_junit_or_run_tests(self):
        self.env["FIXTURE_FAILURE"] = "discovery fixture-second"
        result = self.run_step("Complete backend project tests")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.receipt("fixture-second", "discovery"), 19)
        self.assertEqual(self.receipt("fixture-first", "test"), 0)
        self.assertFalse((self.output / "fixture-second/test-time.json").exists())
        self.assertFalse((self.output / "fixture-second/junit.xml").exists())
        self.assertFalse(
            (self.root / "codex-rs/target/nextest/local/junit.xml").exists()
        )

    def test_test_failure_and_signal_remain_failed_and_continue_other_projects(self):
        self.env["FIXTURE_FAILURE"] = "test fixture-first"
        for code, expected_exit in ((19, 19), (-1, 143)):
            with self.subTest(code=code):
                self.env["FIXTURE_EXIT"] = str(code)
                result = self.run_step("Complete backend project tests")
                self.assertEqual(result.returncode, 1)
                self.assertEqual(self.receipt("fixture-first", "test"), expected_exit)
                self.assertEqual(self.receipt("fixture-second", "test"), 0)

    def test_clippy_failure_remains_failed_and_continues_other_projects(self):
        result = self.run_step("Complete backend project tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.env["FIXTURE_FAILURE"] = "clippy fixture-first"
        result = self.run_step("Scoped backend Clippy")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.receipt("fixture-first", "clippy"), 19)
        self.assertEqual(self.receipt("fixture-second", "clippy"), 0)


if __name__ == "__main__":
    unittest.main()
