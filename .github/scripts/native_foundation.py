"""Produce finite native-foundation evidence; independent receiving stays with CI."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import time

ROOT = Path.cwd()
EVIDENCE = ROOT / "backend-evidence"
RECIPE = json.loads(
    (Path(__file__).with_name("native_foundation_recipe.json")).read_text()
)
DEFINITION_PATHS = {
    ".github/workflows/rust-ci.yml",
    ".github/scripts/native_foundation.py",
    ".github/scripts/native_foundation_recipe.json",
}


def need(condition, message):
    if not condition:
        raise ValueError(message)


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        need(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load(path):
    return json.loads(path.read_text(), object_pairs_hook=strict_object)


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def git(*args):
    return subprocess.check_output(["git", *args])


def inventory():
    names = set(
        git("ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")
    ) - {b""}
    result = {}
    for raw in sorted(names):
        name = os.fsdecode(raw)
        if name.startswith("backend-evidence/"):
            continue
        path = ROOT / name
        if not path.exists() and not path.is_symlink():
            result[name] = None
            continue
        data = (
            os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        )
        result[name] = {
            "mode": path.lstat().st_mode,
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    return result


def initialize():
    EVIDENCE.mkdir(exist_ok=True)
    head = git("rev-parse", "HEAD").decode().strip()
    need(
        head == os.environ["GITHUB_SHA"] == os.environ["GITHUB_WORKFLOW_SHA"],
        "definition checkout mismatch",
    )
    need(os.environ["GITHUB_EVENT_NAME"] == "workflow_dispatch", "wrong event")
    need(
        os.environ["GITHUB_REF"] == RECIPE["definition_ref"],
        "wrong ref",
    )
    need(
        git("show", "-s", "--format=%P", "HEAD").decode().strip()
        == RECIPE["source_commit"],
        "D must have sole parent C",
    )
    need(
        git("rev-parse", RECIPE["source_commit"] + "^{tree}").decode().strip()
        == RECIPE["source_tree"],
        "wrong C tree",
    )
    changed = set(
        git("diff", "--name-only", RECIPE["source_commit"], "HEAD")
        .decode()
        .splitlines()
    )
    need(changed == DEFINITION_PATHS, "unexpected definition paths or product changes")
    save(EVIDENCE / "source-before.json", inventory())
    (EVIDENCE / "index-before").write_bytes(git("ls-files", "--stage", "-z"))
    save(
        EVIDENCE / "source.json",
        {
            "product_commit": RECIPE["source_commit"],
            "product_tree": RECIPE["source_tree"],
            "definition": head,
            "definition_tree": git("rev-parse", "HEAD^{tree}").decode().strip(),
            "scope": "SCOPED_TUI_CAUSAL_FOCUSED",
            "release_artifact": False,
            "github": {
                key: os.environ.get(key)
                for key in (
                    "GITHUB_RUN_ID",
                    "GITHUB_RUN_ATTEMPT",
                    "GITHUB_JOB",
                    "GITHUB_REF",
                    "GITHUB_EVENT_NAME",
                    "GITHUB_REPOSITORY",
                    "GITHUB_WORKFLOW_REF",
                    "RUNNER_NAME",
                    "RUNNER_OS",
                    "RUNNER_ARCH",
                )
            },
        },
    )


def phase(argv, directory, name, stdout_name=None):
    directory.mkdir(parents=True, exist_ok=True)
    started = time.time()
    command = [
        "/usr/bin/time",
        "-q",
        "-f",
        os.environ["CI_TIME_FORMAT"],
        "-o",
        str(directory / f"{name}-time.json"),
        *argv,
    ]
    receipt = {
        "argv": argv,
        "cwd": str(ROOT / "codex-rs"),
        "started": started,
        "exit": None,
    }
    save(directory / f"{name}-phase.json", receipt)
    with (
        (directory / (stdout_name or f"{name}.stdout")).open("wb") as out,
        (directory / f"{name}.stderr").open("wb") as err,
    ):
        result = subprocess.run(
            command, cwd=ROOT / "codex-rs", stdout=out, stderr=err, check=False
        )
    receipt.update(exit=result.returncode, ended=time.time())
    save(directory / f"{name}-phase.json", receipt)
    (directory / f"{name}-exit.txt").write_text(f"{result.returncode}\n")
    return result.returncode


# Producer-side route2 scope check, extended for the full default TUI suite.
# CI independently receives raw
# discovery/JUnit using its trusted receiver; this function awards no acceptance.
def foundation_roster(full, selected, spec):
    group = spec["id"]
    need(
        group in ("core-all", "core-lib", "app-all", "protocol-all", "tui-all", "tui-causal-focused"),
        "unknown foundation group",
    )
    package = spec["package"]
    fixed = {
        "core-all": ("codex-core", "codex-core::all", "all", "test"),
        "core-lib": ("codex-core", "codex-core", "codex_core", "lib"),
        "tui-causal-focused": ("codex-tui", "codex-tui", "codex_tui", "lib"),
        "app-all": ("codex-app-server", "codex-app-server::all", "all", "test"),
    }
    if group in ("protocol-all", "tui-all"):
        expected_package = "codex-protocol" if group == "protocol-all" else "codex-tui"
        need(
            package == expected_package and not spec["prefixes"],
            "wrong unfiltered package scope",
        )
    else:
        need(package == fixed[group][0], "wrong foundation package")

    def cases(data):
        suites = data["rust-suites"]
        need(isinstance(suites, dict) and suites, "missing native suites")
        if group in fixed:
            need(set(suites) == {fixed[group][1]}, "wrong/missing/extra native target")
        out = {}
        for binary, suite in suites.items():
            need(
                suite["package-name"] == package
                and suite["binary-id"] == binary
                and suite["status"] == "listed",
                "wrong package/binary or unlisted suite",
            )
            if group in fixed:
                need(
                    (suite["package-name"], binary, suite["binary-name"], suite["kind"])
                    == fixed[group],
                    "wrong exact foundation target",
                )
            else:
                need(
                    isinstance(binary, str)
                    and binary
                    and isinstance(suite["binary-name"], str)
                    and suite["binary-name"]
                    and suite["kind"] in ("lib", "test", "bin"),
                    "unknown unfiltered package test target",
                )
            need(isinstance(suite["testcases"], dict), "missing native case table")
            for name, case in suite["testcases"].items():
                need(
                    isinstance(name, str)
                    and name
                    and type(case["ignored"]) is bool
                    and case["filter-match"]["status"] in ("matches", "mismatch"),
                    "unknown case state",
                )
                out[(binary, name)] = case
        return out

    all_cases, filtered = cases(full), cases(selected)
    need(
        set(full["rust-suites"]) == set(selected["rust-suites"]),
        "native target namespace loss",
    )
    # TUI's default suite excludes only these source-pinned ignored helper/manual cases.
    baseline_ignored = set()
    if group == "tui-all":
        baseline_ignored = {
            (row["binary_id"], row["test_name"]) for row in spec["baseline_ignored"]
        }
        need(
            {key for key, case in all_cases.items() if case["ignored"]}
            == baseline_ignored
            and {key for key, case in filtered.items() if case["ignored"]}
            == baseline_ignored,
            "TUI ignored identities differ from pinned product baseline",
        )
    # Derive scope BEFORE excluding mismatched rows; required ignored rows still fail.
    expected = {
        key
        for key in all_cases
        if group in ("protocol-all", "tui-all")
        or key[1].startswith(tuple(spec["prefixes"]))
    }
    if group == "tui-causal-focused":
        expected = {("codex-tui", name) for name in spec["required"]}
        need(len(expected) == 30 and expected <= set(all_cases), "missing exact focused cases")
    expected -= baseline_ignored
    need(expected, "empty foundation scope")
    need(
        all(
            not all_cases[key]["ignored"]
            and all_cases[key]["filter-match"]["status"] == "matches"
            for key in expected
        ),
        "required full-discovery row ignored or mismatched",
    )
    need(
        all(
            any(name.startswith(prefix) for _, name in expected)
            for prefix in spec["prefixes"]
        ),
        "missing complete required namespace",
    )
    names = {name for _, name in expected}
    need(set(spec["required"]) <= names, "required new/reused native anchor missing")
    if group in ("tui-all", "tui-causal-focused"):
        suite = full["rust-suites"].get("codex-tui", {})
        need(
            suite.get("binary-name") == "codex_tui" and suite.get("kind") == "lib",
            "missing TUI library target",
        )
        need(
            all(("codex-tui", name) in expected for name in spec["required"]),
            "required TUI anchor missing from library target",
        )
    for prefix, count in spec["required_family_counts"].items():
        need(
            sum(name.startswith(prefix) for name in names) == count,
            "parameterized native case count differs",
        )
    need(set(filtered) <= set(all_cases), "selected discovery invented native identity")
    actual = {
        key
        for key, case in filtered.items()
        if case["filter-match"]["status"] == "matches" and key not in baseline_ignored
    }
    need(
        actual == expected and all(not filtered[key]["ignored"] for key in expected),
        "partial/extra selected scope or ignored selected case",
    )
    roster = [
        {"binary_id": binary, "test_name": name} for binary, name in sorted(expected)
    ]
    counts = {
        "full_discovered": len(all_cases),
        "full_ignored": sum(case["ignored"] for case in all_cases.values()),
        "full_mismatched": sum(
            case["filter-match"]["status"] == "mismatch" for case in all_cases.values()
        ),
        "required_namespace_rows": len(expected),
        "selected_native_rows": len(filtered),
        "selected_matches": len(actual),
        "selected_ignored": sum(case["ignored"] for case in filtered.values()),
        "selected_mismatched": sum(
            case["filter-match"]["status"] == "mismatch" for case in filtered.values()
        ),
    }
    return roster, counts


def run():
    failed = False
    ready = {}
    # Preserve each source-pinned discovery before any selected alias or test.
    for spec in RECIPE["groups"]:
        directory = ROOT / spec["evidence_directory"]
        ready[spec["id"]] = (
            phase(
                spec["full_discovery_argv"], directory, "discovery", "discovered.json"
            )
            == 0
        )
    for spec in RECIPE["groups"]:
        directory = ROOT / spec["evidence_directory"]
        if not ready[spec["id"]]:
            failed = True
            continue
        if spec["selected_discovery_argv"]:
            ready[spec["id"]] = (
                phase(
                    spec["selected_discovery_argv"],
                    directory,
                    "selected-discovery",
                    "selected.json",
                )
                == 0
            )
        else:
            shutil.copyfile(directory / "discovered.json", directory / "selected.json")
            save(
                directory / "selected-alias.json",
                {
                    "source": "discovered.json",
                    "same_bytes": True,
                    "extra_command": False,
                },
            )
        if ready[spec["id"]]:
            try:
                roster, counts = foundation_roster(
                    load(directory / "discovered.json"),
                    load(directory / "selected.json"),
                    spec,
                )
                save(
                    directory / "producer-roster.json",
                    {"roster": roster, "counts": counts},
                )
            except (ValueError, KeyError, TypeError) as error:
                save(directory / "roster-error.json", {"error": str(error)})
                ready[spec["id"]] = False
        failed |= not ready[spec["id"]]
    for spec in RECIPE["groups"]:
        if not ready[spec["id"]]:
            continue
        directory = ROOT / spec["evidence_directory"]
        junit = ROOT / "codex-rs/target/nextest/local/junit.xml"
        junit.unlink(missing_ok=True)
        code = phase(spec["test_argv"], directory, "test")
        if junit.is_file() and junit.stat().st_size:
            shutil.copyfile(junit, directory / "junit.xml")
        else:
            save(directory / "junit-missing.json", {"missing_fresh_junit": True})
            failed = True
        failed |= code != 0
    # Preserve native failures; fix/format never replace test results or cause reruns.
    for spec in RECIPE["post_test_phases"]:
        failed |= phase(spec["argv"], EVIDENCE / "native-foundation", spec["id"]) != 0
    return int(failed)


def finish():
    before = load(EVIDENCE / "source-before.json")
    after = inventory()
    save(EVIDENCE / "source-after.json", after)
    changed = sorted(
        name
        for name in before.keys() | after.keys()
        if before.get(name) != after.get(name)
    )
    (EVIDENCE / "source.diff").write_bytes(
        git("diff", "HEAD", "--binary", "--full-index")
    )
    (EVIDENCE / "index-after").write_bytes(git("ls-files", "--stage", "-z"))
    index_same = (EVIDENCE / "index-before").read_bytes() == (
        EVIDENCE / "index-after"
    ).read_bytes()
    with tarfile.open(
        EVIDENCE / "source-writeback.tar.gz", "w:gz", dereference=False
    ) as archive:
        for name in changed:
            path = ROOT / name
            if path.exists() or path.is_symlink():
                archive.add(path, arcname=name, recursive=False)
    save(
        EVIDENCE / "source-integrity.json",
        {
            "changed": changed,
            "index_unchanged": index_same,
            "source_unchanged": not changed and index_same,
        },
    )
    need(
        not changed and index_same,
        "source writeback; retain evidence and return to author",
    )


if __name__ == "__main__":
    if sys.argv[1:] == ["initialize"]:
        initialize()
    elif sys.argv[1:] == ["run"]:
        sys.exit(run())
    elif sys.argv[1:] == ["finish"]:
        finish()
    else:
        raise SystemExit("expected initialize, run or finish")
