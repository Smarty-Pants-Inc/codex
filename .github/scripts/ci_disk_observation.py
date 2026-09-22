#!/usr/bin/env python3
"""Bounded, read-only Linux CI disk observations; never delete build evidence."""

import datetime
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading


def snapshot(phase, sizes=False):
    workspace = Path(os.environ.get("GITHUB_WORKSPACE", os.getcwd()))
    target = Path(os.environ.get("CARGO_TARGET_DIR", workspace / "codex-rs/target"))
    cargo = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    temporary = Path(os.environ.get("RUNNER_TEMP", "/tmp"))
    print(f"disk-observation phase={phase} at={datetime.datetime.now(datetime.timezone.utc).isoformat()} target={target}", flush=True)
    roots = [workspace, target, temporary, cargo, Path("/tmp")]
    measurements = [(flag, root) for root in dict.fromkeys(roots) for flag in ("-Pk", "-Pi")]
    if sizes:
        roots = [target, *(target / "debug" / name for name in ("deps", "build", "incremental")), cargo / "registry", cargo / "git", temporary, Path("/tmp")]
        measurements.extend(("size", root) for root in dict.fromkeys(roots))
    for flag, root in measurements:
        if not root.exists():
            print(f"disk-observation missing={root}", flush=True)
            continue
        command = ["du", "-sx", "--block-size=1", "--", str(root)] if flag == "size" else ["df", flag, "--", str(root)]
        try:
            # Fixed roots and summary-only commands; no filenames or file contents.
            result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
            print(f"disk-observation command={command[0]} mode={flag} root={root} exit={result.returncode}", flush=True)
            print(result.stdout[:4096].decode(errors="replace"), end="", flush=True)
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"disk-observation unavailable mode={flag} root={root} reason={type(error).__name__}", flush=True)
    # Parent/child and shared-filesystem measurements overlap; do not sum them.


def observe(phase, sizes=False):
    try:
        snapshot(phase, sizes=sizes)
    except Exception:
        # Even output failure on an exhausted runner must not mask test status.
        pass


def run(command):
    observe("pre-workspace", sizes=True)
    stopped = threading.Event()

    def sample():
        for _ in range(90):
            if stopped.wait(60):
                return
            observe("workspace")

    worker = threading.Thread(target=sample)
    child = subprocess.Popen(command, start_new_session=True)

    def forward(signum, frame):
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass

    previous = {}
    for sig in (signal.SIGTERM, signal.SIGINT):
        previous[sig] = signal.signal(sig, forward)
    worker.start()
    try:
        status = child.wait()
    finally:
        stopped.set()
        worker.join()
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    observe("post-workspace", sizes=True)
    return status if status >= 0 else 128 - status


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "run":
        sys.exit(run(sys.argv[2:]))
    if len(sys.argv) == 2:
        observe(sys.argv[1], sizes=True)
    else:
        sys.exit("usage: ci_disk_observation.py PHASE | run COMMAND [ARG ...]")
