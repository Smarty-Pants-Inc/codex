"""Validate and copy the immutable original corpus, never regenerate it."""
from pathlib import Path
import hashlib
import json
import shutil
import stat
import sys

if not __debug__:
    raise RuntimeError("optimized Python is forbidden")
source, destination = map(Path, sys.argv[1:3])
assert source.is_dir() and not source.is_symlink()
assert not destination.exists() and not destination.is_symlink()
manifest_path = source / "MANIFEST.json"
assert stat.S_ISREG(manifest_path.lstat().st_mode) and manifest_path.stat().st_size <= 65536
raw = manifest_path.read_bytes()
expected = "14cea29a0d559b5424e8ab0c53b3e22adf561353e6eceb0e9c07ea2d59c05c8b"
assert hashlib.sha256(raw).hexdigest() == expected
manifest = json.loads(raw)
assert len(manifest["cases"]) == 8
rows = []
for case in manifest["cases"]:
    for kind in ("input", "body", "text", "frame"):
        row = case[kind]
        name = row["name"]
        assert isinstance(name, str) and Path(name).name == name and name not in (".", "..")
        p = source / name
        assert stat.S_ISREG(p.lstat().st_mode) and p.stat().st_size == row["bytes"] <= 1024**2
        assert hashlib.sha256(p.read_bytes()).hexdigest() == row["sha256"]
        rows.append({"name": name, "bytes": row["bytes"], "sha256": row["sha256"]})
assert len(rows) == len({row["name"] for row in rows}) == 32
assert sum(row["bytes"] for row in rows) + len(raw) <= 2 * 1024**2
assert {p.name for p in source.iterdir()} == {row["name"] for row in rows} | {"MANIFEST.json"}
# Only stage after the whole source corpus passes. No receipts enter the fixture directory.
destination.mkdir(mode=0o700)
try:
    for name in ["MANIFEST.json"] + [row["name"] for row in rows]:
        shutil.copyfile(source / name, destination / name)
        assert (destination / name).read_bytes() == (source / name).read_bytes()
        (destination / name).chmod(0o400)
    destination.chmod(0o500)
except BaseException:
    destination.chmod(0o700)
    shutil.rmtree(destination)
    raise
print(json.dumps({"manifest_sha256": expected, "members": rows,
                  "destination": str(destination), "read_only": True}, indent=2))
