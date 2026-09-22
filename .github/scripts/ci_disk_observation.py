#!/usr/bin/env python3
"""Bounded, read-only Linux CI disk observations; never delete build evidence."""

import datetime
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading


def snapshot(phase, sizes=False, stopped=None):
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
        if stopped is not None and stopped.is_set():
            return
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


def observe(phase, sizes=False, stopped=None):
    try:
        snapshot(phase, sizes=sizes, stopped=stopped)
    except Exception:
        # Even output failure on an exhausted runner must not mask test status.
        pass


def run(command, phase="workspace"):
    stopped = threading.Event()
    cancelled = threading.Event()
    child = None
    pending = []
    worker = None
    worker_started = False
    previous = {}

    def forward(signum, frame):
        cancelled.set()
        stopped.set()
        if child is None:
            pending.append(signum)
        else:
            try:
                os.killpg(child.pid, signum)
            except ProcessLookupError:
                pass

    def sample():
        for _ in range(90):
            if stopped.wait(60):
                return
            observe(phase, stopped=stopped)

    try:
        for sig in (signal.SIGTERM, signal.SIGINT):
            previous[sig] = signal.signal(sig, forward)
        if pending:
            return 128 + pending[0]
        child = subprocess.Popen(command, start_new_session=True)
        for signum in pending:
            forward(signum, None)
        try:
            worker = threading.Thread(target=sample)
            worker.start()
            worker_started = True
        except Exception:
            # Sampling is optional; the test child must still be waited/reaped.
            pass
        status = child.wait()
        stopped.set()
        if worker_started:
            worker.join()
            worker_started = False
        if not cancelled.is_set():
            observe(f"post-{phase}", sizes=True, stopped=cancelled)
        return status if status >= 0 else 128 - status
    finally:
        stopped.set()
        if child is not None and child.poll() is None:
            forward(signal.SIGTERM, None)
            child.wait()
        if worker_started:
            worker.join()
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "run":
        sys.exit(run(sys.argv[3:], phase=sys.argv[2]))
    if len(sys.argv) == 2:
        observe(sys.argv[1], sizes=True)
    else:
        sys.exit("usage: ci_disk_observation.py PHASE | run PHASE COMMAND [ARG ...]")
