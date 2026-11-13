#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
python3 -B - <<'PY'
import hashlib
import importlib.util
import io
import subprocess
from unittest.mock import patch

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

metadata = load("relan_metadata", "tests/walk_support/relan-metadata.py")
producer = load("relan_produce", "tests/guest_support/relan-produce.py")
flags = "Read-only: yes\nHidden: yes\nSystem: yes\nArchive: yes\nVolume: no\nDirectory: yes\n"
assert metadata.parse_attributes(flags) == 0x37
for invalid in (flags.replace("Volume: no\n", ""), flags + "Hidden: no\n",
                flags.replace("yes", "maybe", 1), flags + "Unknown: no\n"):
    try:
        metadata.parse_attributes(invalid)
    except ValueError:
        pass
    else:
        raise AssertionError(f"invalid flag evidence accepted: {invalid!r}")

listing = "# heading\nf\t1\t777\t0\t0\tabcd\tÜnïcödé path\n"
with patch("builtins.open", return_value=io.StringIO(listing)), patch.object(
    metadata.subprocess, "check_output", side_effect=[flags, "-\n", "LABEL-Δ\n"]
) as call:
    result = metadata.metadata("/own-loop", "listing", 1, "/own-mount", True)
    assert result == {"attributes": {"Ünïcödé path": 0x37},
                      "label": "LABEL-Δ", "birth_unavailable": 1}, result
    assert call.call_args_list[0].args[0][-1] == "/Ünïcödé path"
    assert call.call_args_list[1].args[0][-1] == "/own-mount/Ünïcödé path"
for response, error in (("now supported", RuntimeError),
                        (subprocess.CalledProcessError(1, "stat"), subprocess.CalledProcessError)):
    with patch("builtins.open", return_value=io.StringIO(listing)), patch.object(
        metadata.subprocess, "check_output", side_effect=[flags, response]
    ):
        try:
            metadata.metadata("/own-loop", "listing", 1, "/own-mount", True)
        except error:
            pass
        else:
            raise AssertionError("unexpected birth-time evidence must fail")

for cluster in (4096, 131072):
    created = producer.model(cluster, "create")
    truncated = producer.model(cluster, "truncate")
    renamed = producer.model(cluster, "rename")
    cleared = producer.model(cluster, "empty-label")
    assert len(created["entries"]) == 23 + producer.many_count(cluster)
    assert len(truncated["entries"]) == len(created["entries"]) - 4
    assert len(renamed["entries"]) == len(truncated["entries"]) - 2
    assert truncated["entries"]["Relan/gap.bin"]["size"] == 350001
    assert truncated["entries"]["Relan/empty"]["sha256"] == hashlib.sha256(bytes(50001)).hexdigest()
    assert renamed["entries"]["Relan/replaced.bin"]["sha256"] == hashlib.sha256(b"plain ASCII contents\n").hexdigest()
    assert renamed["entries"]["Relan/CASE.TXT"]["attributes"] == 0x27
    assert "keep/Moved directory/large.bin" in renamed["entries"]
    assert renamed["entries"]["keep/sentinel.bin"] == created["entries"]["keep/sentinel.bin"]
    assert cleared["entries"] == renamed["entries"] and cleared["label"] == ""
print("PASS  relan metadata refuses missing evidence; write models retain all paths")
PY
