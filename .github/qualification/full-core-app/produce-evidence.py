"""Proposed bounded DATA producer; no upload, test execution or qualification award."""
from pathlib import Path
import hashlib
import json
import re
import shutil
import stat
import sys

if not __debug__:
    raise RuntimeError("optimized Python is not permitted for evidence validation")

root, out = (Path(value).resolve(strict=True) for value in sys.argv[1:3])
lane = sys.argv[3]
phases = {"core": ["core"], "app-server": ["app-server"]}[lane]
assert root != out and root not in out.parents
limit_file, limit_total, limit_snapshots = 16 * 1024**2, 32 * 1024**2, 256
base = {"source-before.txt", "source-after.txt", "source.patch", "index.patch", "status-after.txt",
        "lane-exit.txt", "resources-before.txt", "disk-after.txt", "dependencies.sha256", "tools.log",
        "MODULE.bazel.lock", "lane.txt", "test-environment.txt", "test-fix-fmt-phases.txt", "failure-classification.txt", "final-clean.txt"}
allowed = set(base)
for phase in phases + ["fix", "fmt"]:
    allowed.update({phase + ".command.txt", phase + ".log"})
for phase in phases:
    allowed.update({"inventory-" + phase + suffix for suffix in [".command.txt", ".json", ".stderr"]})
entries = list(out.iterdir())
assert all(p.name in allowed and stat.S_ISREG(p.lstat().st_mode) and p.stat().st_size <= limit_file for p in entries)
assert sum(p.stat().st_size for p in entries) <= limit_total
exit_code = int((out / "lane-exit.txt").read_text())
clean = exit_code == 0
inventories = {}
for phase in phases:
    path = out / ("inventory-" + phase + ".json")
    if not path.exists() or not path.stat().st_size:
        assert not clean
        inventories[phase] = {"available": False}
        continue
    try:
        suites = json.loads(path.read_bytes())["rust-suites"]
        selected = sorted(
            (suite_id, name)
            for suite_id, suite in suites.items()
            for name, case in suite.get("testcases", {}).items()
            if case.get("filter-match", {}).get("status") == "matches" and not case["ignored"]
        )
        assert selected
        names = [name for _, name in selected]
        if phase == "core":
            for module in ["hooks", "observation_retry", "observation_transport", "observation_wake", "native_control_boundary", "native_pilot_producers"]:
                assert any(name.startswith("suite::" + module + "::") for name in names), module
            for fragment in ["observation::tests::budget_tests::", "observation::tests::wake_tests::", "snapshot_tests::", "session::observation_budget::tests::", "pilot_installation_after_ordinary_prepare", "cancelled_count_retains_debit"]:
                assert any(fragment in name for name in names), fragment
        if phase == "app-server":
            for fragment in ["trusted_policy_validation_receipts_and_inflight_cleanup_use_original_native_paths", "observation_start_and_admitted_resume_install_fresh_owned_relays"]:
                assert any(fragment in name for name in names), fragment
        inventories[phase] = {"available": True, "selected_nonignored": len(selected), "cases": selected}
    except (AssertionError, KeyError, TypeError, ValueError) as error:
        assert not clean, f"required actual inventory invalid: {phase}: {error}"
        inventories[phase] = {"available": True, "valid": False, "classification": "partial-or-unrecognized-inventory"}
    if clean:
        log = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", (out / (phase + ".log")).read_text())
        assert re.search(r"Summary .*\b[1-9][0-9]* tests? run:", log), phase
        assert "PASS" in log, phase
if clean:
    for name in allowed - {"failure-classification.txt"}:
        assert (out / name).is_file(), name
    assert (out / "source-before.txt").read_bytes() == (out / "source-after.txt").read_bytes()
    for name in ["source.patch", "index.patch", "status-after.txt"]:
        assert (out / name).read_bytes() == b""
# Only the selected package's snapshot candidates; full bytes, never auto-accept.
snapshots = []
logical_size = sum(p.stat().st_size for p in entries)
if lane in ("core", "app-server"):
    for path in sorted((root / "codex-rs" / lane).rglob("*.snap.new")):
        assert stat.S_ISREG(path.lstat().st_mode) and path.stat().st_size <= limit_file
        assert root in path.resolve(strict=True).parents
        assert len(snapshots) < limit_snapshots
        logical_size += path.stat().st_size
        assert logical_size <= limit_total
        relative = path.relative_to(root)
        target = out / "snapshots" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        snapshots.append(str(target.relative_to(out)))
coverage = {"lane": lane, "lane_exit": exit_code, "source_clean": clean, "inventories": inventories,
            "snapshot_candidates": snapshots, "qualification_awarded": False,
            "semantic_acceptance_pending": "CI must join actual named results/counts/skips against each selected inventory; any PASS/summary is not per-test acceptance",
            "limits": {"per_file_bytes": limit_file, "logical_bytes": limit_total, "snapshots": limit_snapshots}}
(out / "coverage.json").write_text(json.dumps(coverage, indent=2) + "\n")
files = sorted(p for p in out.rglob("*") if p.is_file())
assert all(not p.is_symlink() and p.stat().st_size <= limit_file for p in files)
assert sum(p.stat().st_size for p in files) <= limit_total
manifest = [{"path": str(p.relative_to(out)), "bytes": p.stat().st_size,
             "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in files]
encoded = (json.dumps(manifest, indent=2) + "\n").encode()
assert len(encoded) <= limit_file and len(encoded) + sum(p.stat().st_size for p in files) <= limit_total
(out / "members.json").write_bytes(encoded)
