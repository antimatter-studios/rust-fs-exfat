#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
python3 -B - <<'PY'
import importlib.util
import subprocess
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "birth_times", "tests/write_support/birth-times.py"
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
tree = [("/mount", ["Ünïcödé 目录"], ["dated.txt"])]
with patch.object(module.os, "walk", return_value=tree), patch.object(
    module.subprocess,
    "check_output",
    side_effect=[
        "2001-02-03 13:35:06.780000000 +0930\n",
        "2001-02-02 23:05:06.780000000 -0500\n",
    ],
) as stat:
    actual = module.birth_times("/mount")
    assert actual == {"Ünïcödé 目录": 981173106780, "dated.txt": 981173106780}, actual
    assert stat.call_args_list[0].args[0] == [
        "stat", "--format=%w", "--", "/mount/Ünïcödé 目录"
    ]

for response in ["-\n", "not a timestamp\n"]:
    with patch.object(module.os, "walk", return_value=tree), patch.object(
        module.subprocess, "check_output", return_value=response
    ):
        try:
            module.birth_times("/mount")
        except (RuntimeError, ValueError):
            pass
        else:
            raise AssertionError(f"invalid birth time accepted: {response!r}")

with patch.object(module.os, "walk", return_value=tree), patch.object(
    module.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, "stat")
):
    try:
        module.birth_times("/mount")
    except subprocess.CalledProcessError:
        pass
    else:
        raise AssertionError("a failed stat command must fail the comparison")
print("PASS  kernel birth times preserve hundredths and offsets and refuse missing evidence")
PY
