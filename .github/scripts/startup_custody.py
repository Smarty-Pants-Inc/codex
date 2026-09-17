"""Same-job tool custody for the focused startup definition; no prior inventory assumed."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys


def file_identity(path):
    path = Path(path)
    chain = []
    for _ in range(32):
        if not path.is_symlink():
            break
        target = os.readlink(path)
        chain.append({"path": str(path), "target": target})
        path = Path(os.path.abspath(path.parent / target))
    else:
        raise ValueError("tool symlink depth")
    real = path.resolve(strict=True)
    before = real.stat()
    if not real.is_file():
        raise ValueError("nonregular tool")
    digest = hashlib.sha256(real.read_bytes()).hexdigest()
    after = real.stat()
    fields = ("st_dev", "st_ino", "st_mode", "st_uid", "st_gid", "st_size", "st_mtime_ns", "st_ctime_ns")
    if any(getattr(before, key) != getattr(after, key) for key in fields):
        raise ValueError("tool changed while hashing")
    return {"path": str(Path(path).absolute()), "realpath": str(real), "links": chain,
            "bytes": before.st_size, "sha256": digest, "mode": before.st_mode}


def observe(run, save, evidence):
    """Resolve installed tools once; use resolved Cargo/toolchain/plugin thereafter."""
    result = {"tools": {}, "versions": {}, "rustup": {}, "limitations":
              "File/proxy/version custody on the admitted disposable image, not full loader/shared-library closure."}
    names = ["rustup", "cargo", "rustc", "cargo-nextest", "just", "uv", "sccache",
             "git", "cc", "c++", "ld", "ar", "pkg-config", "curl", "python3", "time"]
    for name in names:
        found = "/usr/bin/time" if name == "time" else shutil.which(name)
        if not found:
            raise ValueError("missing tool: " + name)
        record = file_identity(found)
        record["invoked_path"] = found
        result["tools"][name] = record
    save(evidence / "tools-observed.json", result)
    for tool in ("cargo", "rustc"):
        output = evidence / ("rustup-which-" + tool + ".stdout")
        run("rustup-which-" + tool,
            [result["tools"]["rustup"]["invoked_path"], "which", "--toolchain", "1.95.0", tool])
        real = output.read_text().strip()
        if not Path(real).is_absolute():
            raise ValueError("nonabsolute rustup toolchain result")
        result["rustup"][tool] = {"proxy": result["tools"][tool], "selected": file_identity(real)}
        result["tools"][tool] = {**file_identity(real), "invoked_path": real}
    commands = {
        "rustc": (["-Vv"], ["rustc 1.95.0", "commit-hash: 59807616e1fa2540724bfbac14d7976d7e4a3860", "host: x86_64-unknown-linux-gnu"]),
        "cargo": (["-V"], ["cargo 1.95.0", "f2d3ce0bd"]),
        "cargo-nextest": (["nextest", "--version"], ["release: 0.9.103", "commit-hash: d2e7b879fb79975e8b47a8e3ce569b651e6381c0", "host: x86_64-unknown-linux-gnu"]),
        "just": (["--version"], ["just 1.51.0"]),
        "uv": (["--version"], ["uv 0.11.3"]),
        "sccache": (["--version"], ["sccache 0.7.5"]),
        "time": (["--version"], ["GNU"]),
    }
    for name in names:
        commands.setdefault(name, (["--version"], []))
    for name, (tail, expected) in commands.items():
        run("version-" + name, [result["tools"][name]["invoked_path"], *tail])
        text = (evidence / ("version-" + name + ".stdout")).read_text()
        if not text.strip() or not all(item in text for item in expected):
            raise ValueError("version/target mismatch: " + name)
        for item in expected:
            if ": " in item and item not in text.splitlines():
                raise ValueError("nonexact version/target field")
        versions = {"rustc": "1.95.0", "cargo": "1.95.0", "just": "1.51.0", "uv": "0.11.3", "sccache": "0.7.5"}
        if name in versions and text.splitlines()[0].split()[:2] != [name, versions[name]]:
            raise ValueError("nonexact version token")
        result["versions"][name] = text
    result["bootstrap_python"] = file_identity(sys.executable)
    save(evidence / "tools-observed.json", result)
    return result


def recheck(tools):
    for row in tools["tools"].values():
        current = file_identity(row["invoked_path"])
        if any(current[key] != row[key] for key in ("realpath", "bytes", "sha256", "mode", "links")):
            raise ValueError("tool identity drift")
    for row in tools["rustup"].values():
        proxy = row["proxy"]
        current = file_identity(proxy["invoked_path"])
        if current["sha256"] != proxy["sha256"] or current["realpath"] != proxy["realpath"]:
            raise ValueError("rustup proxy drift")
