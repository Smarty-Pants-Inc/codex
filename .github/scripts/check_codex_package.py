#!/usr/bin/env python3
"""Check an assembled Linux CLI archive before a model or shared runtime uses it."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

sys.path.insert(0, str(Path(os.environ["CODEX_REPO_ROOT"]) / "scripts"))

from codex_package.layout import validate_package_dir
from codex_package.targets import PACKAGE_VARIANTS, TARGET_SPECS


def check_package(archive_path: Path, target: str) -> dict:
    spec = TARGET_SPECS[target]
    if not spec.is_linux:
        raise ValueError("This preflight requires a native Linux CLI package")
    with archive_path.open("rb") as stream:
        archive_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    result = {"archive_sha256": archive_sha256, "target": target, "checks": []}
    with tempfile.TemporaryDirectory(prefix="codex-package-preflight-") as temporary:
        root = Path(temporary)
        package = root / "package"
        package.mkdir()
        with tarfile.open(archive_path, "r:gz") as archive:
            members = []
            size = 0
            for member in archive:
                size += member.size
                if len(members) >= 64 or size > 4 * 1024**3:
                    raise ValueError("Package exceeds the native preflight size limit")
                if not (member.isfile() or member.isdir()):
                    raise ValueError("Package must contain regular files/directories")
                members.append(member)
            archive.extractall(package, members=members, filter="data")
        validate_package_dir(package, PACKAGE_VARIANTS["codex"], spec, include_zsh=True)
        source = json.loads((package / "source.json").read_text())
        if source["target"] != target:
            raise ValueError("Package target differs from its source receipt")
        result["source"] = source
        for name in ("home", "codex-home", "tmp", "work"):
            (root / name).mkdir(mode=0o700)
        env = {
            "PATH": os.defpath,
            "HOME": str(root / "home"),
            "CODEX_HOME": str(root / "codex-home"),
            "TMPDIR": str(root / "tmp"),
            "LANG": "C.UTF-8",
            "TERM": "dumb",
        }
        commands = [
            ("bin/codex", ["--version"]),
            ("bin/codex-code-mode-host", ["--listen", "stdio"]),
            ("codex-resources/bwrap", ["--version"]),
            ("codex-path/rg", ["--version"]),
            ("codex-resources/zsh/bin/zsh", ["--version"]),
        ]
        for relative, arguments in commands:
            binary = package / relative
            with binary.open("rb") as stream:
                binary_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
            completed = subprocess.run(
                [str(binary), *arguments],
                cwd=root / "work",
                env=env,
                input="",
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            )
            result["checks"].append(
                {
                    "binary": relative,
                    "sha256": binary_sha256,
                    "arguments": arguments,
                    "exit": completed.returncode,
                    "stdout": completed.stdout,
                    "stderr": completed.stderr,
                }
            )
        # Exercise Codex's own digest guard before namespace creation. A missing
        # embedded digest must fail this check even if bwrap itself can execute.
        bwrap = package / "codex-resources/bwrap"
        original_size = bwrap.stat().st_size
        with bwrap.open("rb") as stream:
            original_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
        try:
            with bwrap.open("ab") as stream:
                stream.write(b"\0")
            with bwrap.open("rb") as stream:
                tampered_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
            checked = subprocess.run(
                [
                    str(package / "bin/codex"),
                    "sandbox",
                    "--permission-profile",
                    ":read-only",
                    "--",
                    "/bin/true",
                ],
                cwd=root / "work",
                env={**env, "PATH": str(package / "codex-path")},
                input="",
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            expected = (
                f"bundled bubblewrap digest mismatch for {bwrap}: "
                f"expected sha256:{original_sha256}, got sha256:{tampered_sha256}"
            )
            if checked.returncode != 8 or expected not in checked.stderr:
                raise RuntimeError(
                    "Packaged Codex did not enforce the exact bundled bwrap digest: "
                    f"exit={checked.returncode}, stderr={checked.stderr}"
                )
            result["bwrap_digest_binding"] = {
                "original_sha256": original_sha256,
                "tampered_sha256": tampered_sha256,
                "exit": checked.returncode,
                "stderr": checked.stderr,
                "namespace_execution_proven": False,
            }
        finally:
            # Only the disposable extraction is modified, never the archive.
            with bwrap.open("r+b") as stream:
                stream.truncate(original_size)
            with bwrap.open("rb") as stream:
                if hashlib.file_digest(stream, "sha256").hexdigest() != original_sha256:
                    raise RuntimeError("Disposable bwrap restoration failed")
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("target", choices=TARGET_SPECS)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    try:
        receipt = {"success": True, **check_package(args.archive, args.target)}
    except Exception as error:
        args.evidence.write_text(
            json.dumps({"success": False, "error": str(error)}, indent=2) + "\n"
        )
        raise
    args.evidence.write_text(json.dumps(receipt, indent=2) + "\n")
