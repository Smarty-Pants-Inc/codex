"""Fail closed on nextest's actual exact-target selected inventory."""
from pathlib import Path
import json
import sys

if not __debug__:
    raise RuntimeError("optimized Python is forbidden")
path, selection, index = sys.argv[1:4]
check = json.loads(Path(selection).read_bytes())["named_checks"][int(index)]
file = Path(path)
assert not file.is_symlink() and file.stat().st_size <= 16 * 1024**2
payload = json.loads(file.read_bytes())
selected = []
for suite_id, suite in payload["rust-suites"].items():
    for name, case in suite.get("testcases", {}).items():
        if case["filter-match"]["status"] == "matches":
            selected.append((suite_id, suite, name, case))
assert len(selected) == 1
suite_id, suite, name, case = selected[0]
expected_id = check["package"] + ("::all" if check["target"] == "all" else "")
assert suite_id == suite["binary-id"] == expected_id
assert suite["package-name"] == check["package"]
assert suite["kind"] == ("lib" if check["target"] == "lib" else "test")
assert suite["binary-name"] == (check["package"].replace("-", "_") if check["target"] == "lib" else "all")
assert suite["status"] == "listed" and name == check["name"]
assert case["ignored"] is (check["run_ignored"] == "only")
print(json.dumps({"binary": expected_id, "name": name, "ignored": case["ignored"], "selected": 1}))
