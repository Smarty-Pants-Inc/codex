"""Require each named case/family in the job's compiled nextest selection."""

import argparse
import json
from pathlib import Path
import re


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory", type=Path)
    parser.add_argument("required", nargs="+", help="regex over package::test-name")
    args = parser.parse_args()
    if args.inventory.stat().st_size > 16 * 1024 * 1024:
        parser.error("nextest inventory exceeds 16 MiB")
    inventory = json.loads(args.inventory.read_text())
    selected = {
        f"{suite['package-name']}::{name}"
        for suite in inventory["rust-suites"].values()
        if suite["status"] == "listed"
        for name, case in suite["testcases"].items()
        if case["filter-match"]["status"] == "matches" and not case["ignored"]
    }
    missing = []
    for required in args.required:
        matches = sorted(name for name in selected if re.search(required, name))
        print(json.dumps({"required": required, "count": len(matches), "sample": matches[:3]}))
        if not matches:
            missing.append(required)
    if missing:
        parser.error("missing required selected tests: " + ", ".join(missing))
    print(json.dumps({"compiled_selected": len(selected), "runtime_executed": False}))


if __name__ == "__main__":
    main()
