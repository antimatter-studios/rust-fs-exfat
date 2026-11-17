#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$REPO" <<'PY'
import pathlib
import re
import sys

import yaml

root = pathlib.Path(sys.argv[1])
chores = yaml.safe_load((root / "chores.yml").read_text())
config = (root / "fs-linux-test-harness.toml").read_text()
assert chores["vars"]["LINUX_HARNESS_REF"] == "v0.7.0", "pin the shared-guest release"
assert not re.search(r"^\s*\[vm\]\s*(?:#.*)?$", config, re.MULTILINE), \
    "consumer VM settings do not configure the shared guest"
for task in ("fixtures", "test:vm"):
    timeout = chores["tasks"][task]["on_timeout"]
    assert timeout == [{"cmd": "../fs-linux-test-harness/scripts/vm.sh reap"}], \
        f"a {task} timeout must not stop another consumer's guest"
assert "session cargo test" in (root / "scripts/test.sh").read_text(), \
    "each test tier must retain its own harness session"
PY
echo "shared guest: config and lifecycle agree"
