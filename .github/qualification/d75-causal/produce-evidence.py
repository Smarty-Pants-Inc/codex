"""Bounded manifest-bound DATA producer. Partial evidence never awards a phase."""
from pathlib import Path
import hashlib
import json
import re
import stat
import subprocess
import sys

if not __debug__:
    raise RuntimeError("optimized Python is forbidden")
out = Path(sys.argv[1]).resolve(strict=True)
lane = sys.argv[2]
assert lane in ("core", "app")
recipe = Path(__file__).resolve().parent
selection = json.loads((recipe / "SELECTION.json").read_bytes())
package = "codex-core" if lane == "core" else "codex-app-server"
phases = ["source", "source-status", "resources", "tools"]
if lane == "core":
    phases += ["fixtures"]
phases += [f"prerequisite-{i}" for i in range(3)]
phases += [f"compile-{i}" for i, argv in enumerate(selection["compile_argv"]) if argv[3] == package]
for i, check in enumerate(selection["named_checks"]):
    if check["package"] == package:
        phases += [f"inventory-{i}", f"inventory-proof-{i}", f"test-{i}"]
phases += ["final-status", "final-source"]
allowed = {"custody.json", "phases.json", "source-locks.json", "environment.json", "native-executables.json", "lane-exit.txt", "external-lane-exit.txt", "elapsed-seconds.txt", "failure.txt", "wrapper.stdout", "wrapper.stderr", "members.json", "coverage.json"}
allowed |= {name + ext for name in phases for ext in (".stdout", ".stderr")}
files = list(out.iterdir())
assert len(files) < 254
assert all(p.name in allowed and stat.S_ISREG(p.lstat().st_mode) and p.stat().st_size <= 16 * 1024**2 for p in files)
assert sum(p.stat().st_size for p in files) <= 32 * 1024**2
custody = json.loads((out / "custody.json").read_bytes())
assert custody["product"] == selection["candidate"]["head"]
assert custody["tree"] == selection["candidate"]["tree"]
assert custody["parent"] == selection["candidate"]["parent"]
assert custody["repository"] == "Smarty-Pants-Inc/codex" and custody["repository_id"] == "1047329867"
assert custody["attempt"] == "1" and custody["lane"] == lane
assert custody["workflow_ref"] == "Smarty-Pants-Inc/codex/.github/workflows/smarty-ci.yml@refs/heads/qualify/codex-d75-causal"
for key in ("wrapper", "workflow_sha256"):
    assert re.fullmatch(r"[0-9a-f]{40}" if key == "wrapper" else r"[0-9a-f]{64}", custody[key])
external = int((out / "external-lane-exit.txt").read_text())
records = json.loads((out / "phases.json").read_bytes()) if (out / "phases.json").exists() else []
assert [r["name"] for r in records] == phases[:len(records)]
if (out / "source.stdout").exists() and (out / "source.stdout").stat().st_size:
    assert (out / "source.stdout").read_text().strip().split() == [custody["product"], custody["tree"], custody["parent"]]
if any(r["name"].startswith("compile-") for r in records):
    proof = json.loads((out / "native-executables.json").read_bytes())
    assert len(proof) == 3
    for row, (pkg, binary) in zip(proof, [("codex-rmcp-client", "test_stdio_server"), ("codex-code-mode-host", "codex-code-mode-host"), ("codex-cli", "codex")]):
        assert row["package"] == pkg and row["binary"] == binary
        assert row["path"] == selection["layout"][lane]["CARGO_TARGET_DIR"] + "/debug/" + binary
        assert 64 <= row["bytes"] <= 1024**3 and re.fullmatch(r"[0-9a-f]{64}", row["sha256"])
    assert (out / "source-locks.json").read_bytes() == (recipe / "SOURCE-LOCKS.json").read_bytes()
    environment = json.loads((out / "environment.json").read_bytes())
    for key, value in selection["common_env"].items():
        assert environment[key] == value
    if lane == "core":
        fixture = json.loads((out / "fixtures.stdout").read_bytes())
        original = json.loads((recipe / "fixtures/MANIFEST.json").read_bytes())
        expected = [{"name": c[k]["name"], "bytes": c[k]["bytes"], "sha256": c[k]["sha256"]} for c in original["cases"] for k in ("input", "body", "text", "frame")]
        assert fixture["members"] == expected and fixture["read_only"] is True
        assert fixture["manifest_sha256"] == "14cea29a0d559b5424e8ab0c53b3e22adf561353e6eceb0e9c07ea2d59c05c8b"
        assert fixture["destination"] == "/tmp/codex-d75-causal/core/fixtures"
for i, record in enumerate(records):
    assert record["exit"] is None or type(record["exit"]) is int
    if record["exit"] != 0:
        assert i == len(records) - 1 and external != 0
    assert record["cwd"] in (selection["layout"][lane]["cwd"], str(Path(selection["layout"][lane]["cwd"]).parent))
    if record["name"].startswith("compile-"):
        assert record["argv"] == selection["compile_argv"][int(record["name"].split("-")[1])]
    for prefix, key in (("inventory-", "inventory_argv"), ("test-", "argv")):
        if record["name"].startswith(prefix) and record["name"][len(prefix):].isdigit():
            assert record["argv"] == selection["named_checks"][int(record["name"][len(prefix):])][key]
    if record["exit"] is not None:
        assert all((out / (record["name"] + ext)).is_file() for ext in (".stdout", ".stderr"))
    if record["name"].startswith("inventory-proof-") and record["exit"] == 0:
        index = int(record["name"].split("-")[-1])
        verified = subprocess.check_output([sys.executable, str(recipe / "check-inventory.py"), str(out / f"inventory-{index}.stdout"), str(recipe / "SELECTION.json"), str(index)])
        assert json.loads(verified) == json.loads((out / (record["name"] + ".stdout")).read_bytes())
    if record["name"].startswith("test-") and record["exit"] == 0:
        log = ''.join((out / (record["name"] + ext)).read_text() for ext in (".stdout", ".stderr"))
        log = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", log)
        assert re.search(r"Summary[^\n]*\b1 test run:[^\n]*\b1 passed", log)
if external == 0:
    assert len(records) == len(phases) and all(r["exit"] == 0 for r in records)
    assert (out / "lane-exit.txt").read_text().strip() == "0"
    assert (out / "source.stdout").read_bytes() == (out / "final-source.stdout").read_bytes()
    assert not (out / "source-status.stdout").read_bytes() and not (out / "final-status.stdout").read_bytes()
coverage = {"lane": lane, "external_exit": external, "qualification_awarded": False,
            "phase_status": {name: (next((r["exit"] for r in records if r["name"] == name), "UNRUN")) for name in phases},
            "first_failure_stop": True, "source_clean": external == 0, "native_results_require_receiving_review": True}
(out / "coverage.json").write_text(json.dumps(coverage, indent=2) + "\n")
files = sorted(out.iterdir())
assert len(files) + 1 <= 256
assert all(stat.S_ISREG(p.lstat().st_mode) and p.stat().st_size <= 16 * 1024**2 for p in files)
manifest = [{"path": p.name, "bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in files]
raw = (json.dumps(manifest, indent=2) + "\n").encode()
assert len(raw) + sum(p.stat().st_size for p in files) <= 32 * 1024**2
(out / "members.json").write_bytes(raw)
