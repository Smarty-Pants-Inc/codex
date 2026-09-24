import contextlib
import importlib.util
import io
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "disk", Path(__file__).with_name("ci_disk_observation.py")
)
disk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk)


class DiskObservationTests(unittest.TestCase):
    def test_exit_status_and_sampler_cleanup(self):
        for status in (0, 7):
            with (
                self.subTest(status=status),
                patch.object(disk, "snapshot", side_effect=OSError("full")),
            ):
                before = set(threading.enumerate())
                self.assertEqual(
                    disk.run([sys.executable, "-c", f"raise SystemExit({status})"]),
                    status,
                )
                self.assertEqual(set(threading.enumerate()), before)

    def test_phase_label_preserves_command_status(self):
        with patch.object(disk, "observe") as observe:
            status = disk.run(
                [sys.executable, "-c", "raise SystemExit(7)"], phase="full-core"
            )
        self.assertEqual(status, 7)
        self.assertEqual(observe.call_count, 1)
        self.assertEqual(observe.call_args.args, ("post-full-core",))
        self.assertTrue(observe.call_args.kwargs["sizes"])
        self.assertFalse(observe.call_args.kwargs["stopped"].is_set())

    def test_signal_forwarding_and_cleanup(self):
        harness = """
import importlib.util, os, signal, sys, threading
spec = importlib.util.spec_from_file_location('disk', sys.argv[1])
disk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk)
disk.observe = lambda *args, **kwargs: None
threading.Timer(0.5, lambda: os.kill(os.getpid(), signal.SIGTERM)).start()
raise SystemExit(disk.run([sys.executable, '-c', 'import time; time.sleep(20)']))
"""
        result = subprocess.run(
            [sys.executable, "-c", harness, disk.__file__], timeout=5
        )
        self.assertEqual(result.returncode, 128 + signal.SIGTERM)

    def test_signal_during_spawn_is_forwarded(self):
        original = subprocess.Popen

        def spawn(*args, **kwargs):
            child = original(*args, **kwargs)
            os.kill(os.getpid(), signal.SIGTERM)
            return child

        with (
            patch.object(disk, "observe"),
            patch.object(disk.subprocess, "Popen", side_effect=spawn),
        ):
            self.assertEqual(
                disk.run([sys.executable, "-c", "import time; time.sleep(20)"]), 143
            )

    def test_sampler_start_failure_preserves_status(self):
        with (
            patch.object(disk, "observe"),
            patch.object(
                disk.threading.Thread, "start", side_effect=RuntimeError("cannot start")
            ),
        ):
            self.assertEqual(disk.run([sys.executable, "-c", "raise SystemExit(7)"]), 7)

    def test_cancelled_snapshot_stops_between_measurements(self):
        stopped = threading.Event()

        def measure(*args, **kwargs):
            stopped.set()
            return subprocess.CompletedProcess([], 0, b"")

        with (
            patch.object(Path, "exists", return_value=True),
            patch.object(disk.subprocess, "run", side_effect=measure) as command,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            disk.snapshot("cancel", sizes=True, stopped=stopped)
        self.assertEqual(command.call_count, 1)

    def test_bounded_measurements_and_failures(self):
        output = io.StringIO()
        results = [subprocess.TimeoutExpired("df", 5), OSError("unavailable")]
        results.extend([subprocess.CompletedProcess([], 1, b"x" * 5000)] * 20)
        with (
            patch.object(Path, "exists", return_value=True),
            patch.object(disk.subprocess, "run", side_effect=results) as command,
            contextlib.redirect_stdout(output),
        ):
            disk.snapshot("probe", sizes=True)
        self.assertIn("reason=TimeoutExpired", output.getvalue())
        self.assertIn("reason=OSError", output.getvalue())
        self.assertNotIn("x" * 4097, output.getvalue())
        self.assertLessEqual(command.call_count, 18)
        for call in command.call_args_list:
            self.assertEqual(call.kwargs["timeout"], 5)
            self.assertIn(call.args[0][0], ("df", "du"))


if __name__ == "__main__":
    unittest.main()
