"""Bounded startup compile/discovery producer. DATA only; CI receives independently."""
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import tarfile
import time
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parent))
from startup_custody import file_identity, observe, recheck

ROOT = Path.cwd()
EVIDENCE = ROOT / "backend-evidence"
STATE = Path(os.environ["RUNNER_TEMP"]) / "codex-startup-clock.json"


def need(value, reason):
    if not value:
        raise ValueError(reason)


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        need(key not in result, "duplicate key: " + key)
        result[key] = value
    return result


def load(path):
    return json.loads(path.read_bytes(), object_pairs_hook=strict_object)


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


RECIPE = load(Path(__file__).with_name("startup_focused_recipe.json"))
CLOCK = load(STATE)
need(type(CLOCK["start"]) in (int, float) and math.isfinite(CLOCK["start"]), "bad clock")
START = CLOCK["start"]


def remaining(stage):
    end = START + (1200 if stage == "provision" else 6600 if stage == "run" else 7140)
    if stage == "run":
        end = min(end, load(EVIDENCE / "discovery-clock.json")["start"] + 5400)
    return end - time.monotonic()


def git(*args):
    return subprocess.check_output(["/usr/bin/git", *args], timeout=30)


def inventory():
    names = set(git("ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")) - {b""}
    result = {}
    for raw in sorted(names):
        name = os.fsdecode(raw)
        if name.startswith("backend-evidence/"):
            continue
        path = ROOT / name
        if not path.exists() and not path.is_symlink():
            result[name] = None
            continue
        data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        result[name] = {"mode": path.lstat().st_mode, "sha256": hashlib.sha256(data).hexdigest()}
    return result


def phase(directory, tag, argv, stage, env=None):
    """One attempt; process-group cleanup is bounded, not a physical-retirement proof."""
    directory.mkdir(parents=True, exist_ok=True)
    budget = remaining(stage)
    need(budget > 12, "no remaining work/cleanup reserve")
    rec = {"argv": argv, "cwd": str(ROOT / "codex-rs"), "started": time.time(),
           "monotonic_started": time.monotonic(), "budget_seconds": budget, "exit": None}
    receipt = directory / (tag + "-phase.json")
    save(receipt, rec)
    with (directory / (tag + ".stdout")).open("xb") as out, (directory / (tag + ".stderr")).open("xb") as err:
        wrapped = ["/usr/bin/time", "-q", "-f", RECIPE["time_format"], "-o",
                   str(directory / (tag + "-time.json")), *argv]
        process = subprocess.Popen(wrapped, cwd=ROOT / "codex-rs", stdout=out, stderr=err,
                                   env=env, start_new_session=True)
        try:
            code = process.wait(timeout=budget - 10)
        except subprocess.TimeoutExpired:
            rec["timeout"] = True
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(process.pid, sig)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=4)
                    break
                except subprocess.TimeoutExpired:
                    continue
            rec["reaped"] = process.poll() is not None
            code = 124
    rec.update(exit=code, ended=time.time(), monotonic_ended=time.monotonic())
    save(receipt, rec)
    (directory / (tag + "-exit.txt")).write_text(str(code) + "\n")
    need(code == 0 and remaining(stage) > 0, "failed/expired phase: " + tag)


def initialize():
    need(os.environ["GITHUB_RUN_ATTEMPT"] == "1", "no automatic or manual rerun admission")
    need(os.environ["GITHUB_EVENT_NAME"] == "workflow_dispatch" and
         os.environ["GITHUB_REF"] == RECIPE["ref"], "wrong event/ref")
    need(os.environ["GITHUB_REPOSITORY"] == "Smarty-Pants-Inc/codex", "wrong repository")
    need(os.environ["RUNNER_OS"] == "Linux" and os.environ["RUNNER_ARCH"] == "X64", "wrong native route")
    head = git("rev-parse", "HEAD").decode().strip()
    need(head == os.environ["GITHUB_SHA"] == os.environ["GITHUB_WORKFLOW_SHA"], "D checkout mismatch")
    need(git("show", "-s", "--format=%P", "HEAD").decode().strip() == RECIPE["H"], "D sole parent H")
    need(git("rev-parse", RECIPE["H"] + "^{tree}").decode().strip() == RECIPE["H_tree"], "H tree")
    need(set(git("diff", "--name-only", RECIPE["H"], "HEAD").decode().splitlines()) ==
         set(RECIPE["definition_paths"]), "definition-only delta")
    save(EVIDENCE / "source-before.json", inventory())
    (EVIDENCE / "index-before").write_bytes(git("ls-files", "--stage", "-z"))
    save(EVIDENCE / "source.json", {"definition": head,
         "definition_tree": git("rev-parse", "HEAD^{tree}").decode().strip(),
         "product_commit": RECIPE["H"], "product_tree": RECIPE["H_tree"],
         "scope": "STARTUP_COMPILE_DISCOVERY_ONLY", "release_artifact": False,
         "github": {k: os.environ.get(k) for k in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_JOB",
                    "GITHUB_REF", "GITHUB_EVENT_NAME", "GITHUB_REPOSITORY", "RUNNER_NAME", "RUNNER_OS", "RUNNER_ARCH")}})
    save(EVIDENCE / "clock.json", CLOCK)
    need(remaining("provision") > 0, "provision budget already exhausted")


def provision():
    directory = EVIDENCE / "provision"
    run = lambda tag, argv: phase(directory, tag, argv, "provision")
    tools = observe(run, save, directory)
    save(directory / "fetch-environment.json", {"CARGO_NET_RETRY": "0", "CARGO_HTTP_TIMEOUT": "60", "CARGO_NET_GIT_FETCH_WITH_CLI": os.environ.get("CARGO_NET_GIT_FETCH_WITH_CLI"), "cargo": tools["tools"]["cargo"]["invoked_path"]})
    need(tools["bootstrap_python"]["sha256"] == CLOCK["python_sha256"], "bootstrap Python drift")
    v8 = {}
    for variable, expected in RECIPE["v8"].items():
        record = file_identity(os.environ[variable])
        need(record["sha256"] == expected, "V8 retained digest mismatch")
        v8[variable] = record
    save(directory / "v8.json", v8)
    run("system-packages", ["/usr/bin/dpkg-query", "-W", "-f=${binary:Package}\t${Version}\t${Architecture}\n"])
    run("host", ["/usr/bin/uname", "-a"])
    env = dict(os.environ, CARGO_NET_RETRY="0", CARGO_NET_OFFLINE="false", CARGO_HTTP_TIMEOUT="60")
    phase(directory, "locked-fetch", [tools["tools"]["cargo"]["invoked_path"], "fetch", "--locked"], "provision", env)
    env["CARGO_NET_OFFLINE"] = "true"
    phase(directory, "locked-offline-metadata", [tools["tools"]["cargo"]["invoked_path"], "metadata", "--locked", "--offline", "--format-version", "1"], "provision", env)
    # Cargo's locked offline resolution is an availability witness, not compile success.
    metadata = load(directory / "locked-offline-metadata.stdout")
    lock_bytes = (ROOT / "codex-rs/Cargo.lock").read_bytes()
    need(hashlib.sha256(lock_bytes).hexdigest() == RECIPE["lock_sha256"], "lock drift")
    locked = {(p["name"], p["version"], p["source"]): p for p in tomllib.loads(lock_bytes.decode())["package"] if "source" in p}
    packages = []
    for p in metadata["packages"]:
        if p["source"] is None:
            continue
        key = (p["name"], p["version"], p["source"])
        need(key in locked, "metadata package outside lock")
        row = {k: p[k] for k in ("name", "version", "source", "manifest_path")}
        root = Path(p["manifest_path"]).parent
        if p["source"].startswith("registry+"):
            need(root.parent.parent.name == "src" and root.name == p["name"] + "-" + p["version"], "registry source layout")
            archive = root.parents[2] / "cache" / root.parent.name / (root.name + ".crate")
            checksum = file_identity(archive)["sha256"]
            need(checksum == locked[key]["checksum"], "registry archive checksum identity")
            row["registry_package_checksum"] = checksum
        else:
            need(p["source"].startswith("git+"), "unknown dependency source")
            tag = "git-revision-" + str(len(packages))
            run(tag, [tools["tools"]["git"]["invoked_path"], "-C", str(root), "rev-parse", "HEAD"])
            revision = (directory / (tag + ".stdout")).read_text().strip()
            need(revision == p["source"].rsplit("#", 1)[1], "Git revision mismatch")
            row["observed_revision"] = revision
        packages.append(row)
    need({(p["name"], p["version"], p["source"]) for p in packages} == set(locked), "incomplete locked availability")
    save(directory / "available-packages.json", packages)
    recheck(tools)
    need(remaining("provision") > 0, "provision deadline expired")
    save(directory / "complete.json", {"status": "PROVISIONED_DATA", "ended": time.monotonic(),
         "elapsed_seconds": time.monotonic() - START, "tools_sha256": hashlib.sha256((directory / "tools-observed.json").read_bytes()).hexdigest(),
         "image": {k: os.environ.get(k) for k in ("ImageOS", "ImageVersion", "RUNNER_OS", "RUNNER_ARCH")}})


def run_discovery():
    need(load(EVIDENCE / "provision/complete.json")["elapsed_seconds"] <= 1200, "late provisioning")
    save(EVIDENCE / "discovery-clock.json", {"start": time.monotonic()})
    tools = load(EVIDENCE / "provision/tools-observed.json")
    env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_NET_RETRY="0", RUSTUP_TOOLCHAIN="1.95.0")
    need(env.get("SCCACHE_GHA_ENABLED") in ("true", "false"), "unknown cache backend selection")
    if env["SCCACHE_GHA_ENABLED"] == "false":
        need(bool(env.get("SCCACHE_DIR")), "missing local cache directory")
        env.pop("SCCACHE_GHA_ENABLED")
        env.pop("SCCACHE_GHA_VERSION", None)
    for variable, name in {"CARGO": "cargo", "RUSTC": "rustc", "RUSTC_WRAPPER": "sccache", "CC": "cc", "CXX": "c++", "AR": "ar"}.items():
        env[variable] = tools["tools"][name]["invoked_path"]
    env["PATH"] = str(Path(tools["tools"]["cargo-nextest"]["invoked_path"]).parent) + os.pathsep + os.environ["PATH"]
    save(EVIDENCE / "compile-environment.json", {k: env.get(k) for k in RECIPE["captured_environment"]})
    for spec in RECIPE["phases"]:
        recheck(tools)
        argv = [tools["tools"]["cargo"]["invoked_path"], *spec["argv"][1:]]
        phase(EVIDENCE / "startup" / spec["group"], spec["phase"], argv, "run", env)


def finish():
    before = load(EVIDENCE / "source-before.json")
    after = inventory()
    save(EVIDENCE / "source-after.json", after)
    index = git("ls-files", "--stage", "-z")
    (EVIDENCE / "index-after").write_bytes(index)
    changed = sorted(n for n in before.keys() | after.keys() if before.get(n) != after.get(n))
    same = index == (EVIDENCE / "index-before").read_bytes()
    (EVIDENCE / "source.diff").write_bytes(git("diff", "--binary", "HEAD"))
    with tarfile.open(EVIDENCE / "source-writeback.tar.gz", "w:gz", dereference=False) as archive:
        for name in changed:
            path = ROOT / name
            if path.exists() or path.is_symlink():
                archive.add(path, arcname=name, recursive=False)
    save(EVIDENCE / "source-integrity.json", {"changed": changed, "index_unchanged": same, "source_unchanged": not changed and same})
    save(EVIDENCE / "terminal-phases.json", [{"id": p["id"], "status": "RECEIPT_PRESENT" if
         (EVIDENCE / "startup" / p["group"] / (p["phase"] + "-phase.json")).exists() else "UNRUN"} for p in RECIPE["phases"]])
    need(not changed and same, "source/index drift; keep diagnostics")


if __name__ == "__main__":
    command = sys.argv[1]
    EVIDENCE.mkdir(exist_ok=True)
    if command == "guard":
        need(remaining("provision") > int(sys.argv[2]) * 60 + 5, "no room for next provision action")
    else:
        with (EVIDENCE / (command + ".once")).open("x") as marker:
            marker.write(os.environ["GITHUB_RUN_ID"] + "/" + os.environ["GITHUB_RUN_ATTEMPT"])
        {"initialize": initialize, "provision": provision, "run": run_discovery, "finish": finish}[command]()
