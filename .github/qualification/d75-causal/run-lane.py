"""Serial selected native phases. Never invoked by preparation DATA probes."""
from pathlib import Path
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import time

if not __debug__:
    raise RuntimeError("optimized Python is forbidden")
recipe = Path(__file__).resolve().parent
lane = sys.argv[1]
assert lane in ("core", "app")
selection_bytes = (recipe / "SELECTION.json").read_bytes()
assert hashlib.sha256(selection_bytes).hexdigest() == "07624b2d797d483c49483217cc8223530092692f18e312d2fd08a3ee6e45ae27"
selection = json.loads(selection_bytes)
layout = selection["layout"][lane]
root = Path(layout["cwd"]).parent
out = Path(os.environ["D75_EVIDENCE_DIR"]).resolve()
assert out.is_dir() and not out.is_symlink()
assert Path.cwd() == root
for key, value in layout.items():
    if key != "cwd":
        assert os.environ[key] == value
for key, value in selection["common_env"].items():
    assert os.environ[key] == value
assert os.environ["CARGO_INCREMENTAL"] == "0" and os.environ["CARGO_NET_RETRY"] == "0"
package = "codex-core" if lane == "core" else "codex-app-server"
records = []
started = time.monotonic()

def run(name, argv, cwd=None):
    record = {"name": name, "argv": argv, "cwd": str(cwd or root / "codex-rs"),
              "started_unix_ns": time.time_ns(), "exit": None}
    records.append(record)
    (out / "phases.json").write_text(json.dumps(records, indent=2) + "\n")
    begin = time.monotonic()
    with (out / (name + ".stdout")).open("xb") as stdout, (out / (name + ".stderr")).open("xb") as stderr:
        result = subprocess.run(argv, cwd=cwd or root / "codex-rs", stdout=stdout, stderr=stderr, check=False)
    record.update(exit=result.returncode, duration_seconds=time.monotonic() - begin)
    (out / "phases.json").write_text(json.dumps(records, indent=2) + "\n")
    for file in out.iterdir():
        assert file.stat().st_size <= 16 * 1024**2, "evidence file overflow"
    assert sum(file.stat().st_size for file in out.iterdir()) <= 32 * 1024**2, "lane evidence overflow"
    if result.returncode:
        raise RuntimeError(f"first failure {name}: exit {result.returncode}")
    return out / (name + ".stdout")

try:
    identity = run("source", ["git", "show", "-s", "--format=%H %T %P", "HEAD"], root).read_text().strip().split()
    assert identity == [selection["candidate"]["head"], selection["candidate"]["tree"], selection["candidate"]["parent"]]
    assert not run("source-status", ["git", "status", "--porcelain=v1", "--untracked-files=all"], root).read_bytes()
    run("resources", ["bash", "-c", "uname -a; df -Pk .; free -m"], root)
    run("tools", ["bash", "-euc", "rustc -Vv; cargo --version; cargo nextest --version; just --version; python3 --version; cc --version; protoc --version; test \"$(rustc --version | cut -d ' ' -f 2)\" = 1.95.0; test \"$(just --version)\" = 'just 1.51.0'; cargo nextest --version | grep -E '^cargo-nextest 0[.]9[.]103( |$)'" ])
    for file in ("MODULE.bazel", "MODULE.bazel.lock", "codex-rs/Cargo.lock", "codex-rs/Cargo.toml", "codex-rs/rust-toolchain.toml"):
        assert hashlib.sha256((root / file).read_bytes()).hexdigest() == json.loads((recipe / "SOURCE-LOCKS.json").read_text())[file]
    (out / "source-locks.json").write_bytes((recipe / "SOURCE-LOCKS.json").read_bytes())
    env_names = list(selection["common_env"]) + [k for k in layout if k != "cwd"] + ["CARGO_HOME", "RUSTUP_HOME"]
    (out / "environment.json").write_text(json.dumps({k: os.environ[k] for k in env_names}, indent=2) + "\n")
    if lane == "core":
        fixtures = root.parent / "fixtures"
        assert os.environ["CODEX_OBSERVATION_CAPACITY_FIXTURES"] == str(fixtures)
        assert not fixtures.exists() and not fixtures.is_symlink()
        run("fixtures", ["python3", str(recipe / "stage-fixtures.py"), str(recipe / "fixtures"), str(fixtures)])
    binaries = []
    for i, (pkg, binary) in enumerate([("codex-rmcp-client", "test_stdio_server"), ("codex-code-mode-host", "codex-code-mode-host"), ("codex-cli", "codex")]):
        run(f"prerequisite-{i}", ["cargo", "build", "--locked", "-p", pkg, "--bin", binary])
        path = Path(layout["CARGO_TARGET_DIR"]) / "debug" / binary
        before = path.lstat()
        assert stat.S_ISREG(before.st_mode) and os.access(path, os.X_OK)
        assert 64 <= before.st_size <= 1024**3
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            header = stream.read(64)
            assert header[:7] == b"\x7fELF\x02\x01\x01" and int.from_bytes(header[18:20], "little") == 62
            digest.update(header)
            while block := stream.read(1024**2):
                digest.update(block)
        assert path.lstat() == before
        binaries.append({"package": pkg, "binary": binary, "path": str(path), "bytes": before.st_size, "sha256": digest.hexdigest()})
        (out / "native-executables.json").write_text(json.dumps(binaries, indent=2) + "\n")
    for i, argv in enumerate(selection["compile_argv"]):
        if argv[3] == package:
            run(f"compile-{i}", argv)
    for i, check in enumerate(selection["named_checks"]):
        if check["package"] != package:
            continue
        inventory = run(f"inventory-{i}", check["inventory_argv"])
        run(f"inventory-proof-{i}", ["python3", str(recipe / "check-inventory.py"), str(inventory), str(recipe / "SELECTION.json"), str(i)])
        run(f"test-{i}", check["argv"])
        log = (out / f"test-{i}.stdout").read_text() + (out / f"test-{i}.stderr").read_text()
        log = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", log)
        assert re.search(r"Summary[^\n]*\b1 test run:[^\n]*\b1 passed", log), "missing one-test execution proof"
    assert not run("final-status", ["git", "status", "--porcelain=v1", "--untracked-files=all"], root).read_bytes()
    run("final-source", ["git", "show", "-s", "--format=%H %T %P", "HEAD"], root)
    (out / "lane-exit.txt").write_text("0\n")
except Exception as error:
    (out / "failure.txt").write_text(str(error)[:8192] + "\n")
    (out / "lane-exit.txt").write_text("1\n")
    raise
finally:
    (out / "elapsed-seconds.txt").write_text(str(time.monotonic() - started) + "\n")
