#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
python3 -B - <<'PY'
import pathlib
import subprocess
import tempfile

guard = pathlib.Path("scripts/check-test-image-reads.sh").resolve()
subprocess.run(["bash", str(guard)], check=True)
with tempfile.TemporaryDirectory(prefix="exfat-image-read-guard-") as directory:
    root = pathlib.Path(directory)
    tests = root / "tests"
    tests.mkdir()
    allow = tests / "image-reads.allow"
    allow.write_text("")
    source = tests / "planted.rs"
    for read in ('fs::read("disk.img")', 'std::fs::read("disk.img")',
                 'file.read_to_end(&mut bytes)', 'fs ::\nread("disk.img")',
                 'fs::read_to_string("disk.img")', 'file.read_to_string(&mut text)',
                 'use std::fs::read;', 'use std::fs::{File, read};'):
        source.write_text(read + "\n")
        result = subprocess.run(["bash", str(guard), str(root)], capture_output=True, text=True)
        assert result.returncode == 1 and "unbounded read" in result.stderr, result
    source.write_text('fs::read("sector.bin")\n')
    allow.write_text('tests/planted.rs\tfs::read("sector.bin")\t1\t512-byte sector dump\n')
    subprocess.run(["bash", str(guard), str(root)], check=True)
    source.write_text(source.read_text() * 2)
    assert subprocess.run(["bash", str(guard), str(root)], capture_output=True).returncode == 1
    source.write_text("fn no_read() {}\n")
    assert subprocess.run(["bash", str(guard), str(root)], capture_output=True).returncode == 1
    allow.unlink()
    assert subprocess.run(["bash", str(guard), str(root)], capture_output=True).returncode == 1
print("PASS  image reads are bounded; planted reads fail")
PY
