"""Stage only validated manifest-bound DATA. Never upload the original directory."""
from pathlib import Path
import hashlib
import json
import shutil
import stat
import sys

if not __debug__:
    raise RuntimeError("optimized Python is not permitted for upload validation")
source = Path(sys.argv[1]).resolve(strict=True)
requested = Path(sys.argv[2])
assert requested.is_absolute() and not requested.exists() and not requested.is_symlink()
target = requested.parent.resolve(strict=True) / requested.name
assert source != target and source not in target.parents and target not in source.parents
assert not (source / "producer-failed.txt").exists()
manifest_path = source / "members.json"
assert stat.S_ISREG(manifest_path.lstat().st_mode)
raw = manifest_path.read_bytes()
assert len(raw) <= 16 * 1024**2
rows = json.loads(raw)
assert isinstance(rows, list) and 0 < len(rows) <= 320
names = [row["path"] for row in rows]
assert len(names) == len(set(names)) and "members.json" not in names
files = []
for path in source.rglob("*"):
    mode = path.lstat().st_mode
    assert not path.is_symlink()
    assert stat.S_ISREG(mode) or stat.S_ISDIR(mode)
    if stat.S_ISREG(mode):
        files.append(str(path.relative_to(source)))
assert set(files) == set(names) | {"members.json"}
assert len(raw) + sum(row["bytes"] for row in rows) <= 32 * 1024**2
for row in rows:
    relative = Path(row["path"])
    assert not relative.is_absolute() and ".." not in relative.parts
    assert set(row) == {"path", "bytes", "sha256"}
    assert type(row["bytes"]) is int and 0 <= row["bytes"] <= 16 * 1024**2
    path = source / relative
    assert stat.S_ISREG(path.lstat().st_mode) and path.stat().st_size == row["bytes"]
    assert hashlib.sha256(path.read_bytes()).hexdigest() == row["sha256"]
# Only after complete validation create a separate upload directory.
target.mkdir(mode=0o700)
try:
    for row in rows:
        destination = target / row["path"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / row["path"], destination)
        assert destination.stat().st_size == row["bytes"]
        assert hashlib.sha256(destination.read_bytes()).hexdigest() == row["sha256"]
    (target / "members.json").write_bytes(raw)
except BaseException:
    shutil.rmtree(target)
    raise
