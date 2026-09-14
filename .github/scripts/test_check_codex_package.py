import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from check_codex_package import check_package


class PackagePreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.package = self.root / "package"
        self.package.mkdir()
        self.target = "x86_64-unknown-linux-gnu"
        for name in (
            "bin/codex",
            "bin/codex-code-mode-host",
            "codex-path/rg",
            "codex-resources/bwrap",
            "codex-resources/zsh/bin/zsh",
        ):
            binary = self.package / name
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text("inert fixture, never executed")
            binary.chmod(0o755)
        (self.package / "codex-package.json").write_text(
            json.dumps(
                {
                    "layoutVersion": 1,
                    "target": self.target,
                    "variant": "codex",
                    "entrypoint": "bin/codex",
                    "resourcesDir": "codex-resources",
                    "pathDir": "codex-path",
                }
            )
        )
        (self.package / "source.json").write_text(json.dumps({"target": self.target}))

    def archive(self):
        path = self.root / "package.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            archive.add(self.package, arcname=".")
        return path

    @patch("check_codex_package.subprocess.run")
    def test_missing_host_is_rejected_before_any_executable_runs(self, run):
        (self.package / "bin/codex-code-mode-host").unlink()
        with self.assertRaisesRegex(RuntimeError, "codex-code-mode-host"):
            check_package(self.archive(), self.target)
        run.assert_not_called()

    def native_result(self, args, **kwargs):
        if args[1] != "sandbox":
            return subprocess.CompletedProcess(args, 0, "fixture output", "")
        bwrap = Path(args[0]).parent.parent / "codex-resources/bwrap"
        tampered = bwrap.read_bytes()
        original_sha = hashlib.sha256(tampered[:-1]).hexdigest()
        tampered_sha = hashlib.sha256(tampered).hexdigest()
        message = (
            f"bundled bubblewrap digest mismatch for {bwrap}: "
            f"expected sha256:{original_sha}, got sha256:{tampered_sha}"
        )
        return subprocess.CompletedProcess(args, 8, "", message)

    @patch("check_codex_package.subprocess.run")
    def test_complete_package_checks_its_own_binaries_without_credentials(self, run):
        run.side_effect = self.native_result
        receipt = check_package(self.archive(), self.target)
        self.assertEqual(len(receipt["checks"]), 5)
        self.assertEqual(run.call_count, 6)
        self.assertEqual(receipt["bwrap_digest_binding"]["exit"], 8)
        sandbox = run.call_args_list[-1]
        self.assertEqual(
            sandbox.kwargs["env"]["PATH"],
            str(Path(sandbox.args[0][0]).parent.parent / "codex-path"),
        )
        for call, check in zip(run.call_args_list[:5], receipt["checks"], strict=True):
            self.assertTrue(call.args[0][0].endswith("/package/" + check["binary"]))
            self.assertEqual(
                set(call.kwargs["env"]),
                {"PATH", "HOME", "CODEX_HOME", "TMPDIR", "LANG", "TERM"},
            )
            self.assertEqual(call.kwargs["input"], "")
            self.assertTrue(call.kwargs["check"])

    @patch("check_codex_package.subprocess.run")
    def test_missing_or_wrong_embedded_digest_is_rejected(self, run):
        for code, stderr in ((0, ""), (8, "wrong digest")):
            with self.subTest(code=code, stderr=stderr):
                run.side_effect = [
                    *[subprocess.CompletedProcess([], 0, "", "") for _ in range(5)],
                    subprocess.CompletedProcess([], code, "", stderr),
                ]
                with self.assertRaisesRegex(RuntimeError, "exact bundled bwrap digest"):
                    check_package(self.archive(), self.target)

    @patch("check_codex_package.subprocess.run")
    def test_runtime_failure_is_not_a_preflight_pass(self, run):
        run.side_effect = subprocess.CalledProcessError(1, ["fixture"])
        with self.assertRaises(subprocess.CalledProcessError):
            check_package(self.archive(), self.target)


if __name__ == "__main__":
    unittest.main()
