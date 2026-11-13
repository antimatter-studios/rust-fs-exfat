#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
mkdir -p "$REPO/tmp"
python3 - "$REPO" <<'PY'
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile

repo = Path(sys.argv[1])
scripts = (
    "tests/scripts/test-cli-tier.sh",
    "tests/scripts/test-crates-io-check.sh",
    "tests/scripts/test-source-checks.sh",
    "tests/scripts/test-fuzz-all.sh",
    "tests/cli/lib.sh",
)
failures = []
with tempfile.TemporaryDirectory(prefix="mktemp-failure-", dir=repo / "tmp") as directory:
    sandbox = Path(directory)
    tools = sandbox / "bin"
    tools.mkdir()
    marker = sandbox / "failed"
    operations = sandbox / "operations"
    startup = sandbox / "startup.sh"
    startup.write_text('''set -T
guard_after_mktemp() {
    if [[ -f "$MKTEMP_FAILED" && "$BASH_COMMAND" != 'exit 1' ]]; then
        printf '%s\\n' "$BASH_COMMAND" >> "$MKTEMP_OPERATIONS"
        exit 99
    fi
}
trap guard_after_mktemp DEBUG
''')
    temporary_tool = tools / "mktemp"
    temporary_tool.write_text('''#!/bin/sh
printf failed > "$MKTEMP_FAILED"
printf '%s' "$MKTEMP_OUTPUT"
printf '%s\\n' 'mktemp: simulated allocation failure' >&2
exit 73
''')
    temporary_tool.chmod(0o755)
    for command in ("mkdir", "ln", "chmod", "rm", "cp", "mv"):
        executable = shutil.which(command)
        if executable is None:
            raise SystemExit(f"FAIL  {command} is missing; chore tools provides shell test prerequisites")
        stand_in = tools / command
        stand_in.write_text(f'''#!/bin/sh
if [ -f "$MKTEMP_FAILED" ]; then
    printf '%s\\n' '{command}' "$@" >> "$MKTEMP_OPERATIONS"
    exit 99
fi
exec "{executable}" "$@"
''')
        stand_in.chmod(0o755)
    for script in scripts:
        for output in ("", str(sandbox / "not-created")):
            marker.unlink(missing_ok=True)
            operations.write_text("")
            environment = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                               BASH_ENV=str(startup), MKTEMP_FAILED=str(marker),
                               MKTEMP_OPERATIONS=str(operations), MKTEMP_OUTPUT=output)
            result = subprocess.run(["bash", str(repo / script)], cwd=repo,
                                    env=environment, capture_output=True, text=True, timeout=30)
            attempted = operations.read_text().strip()
            if not marker.exists() or "mktemp: simulated allocation failure" not in result.stderr:
                failures.append(f"{script}: the failing mktemp stand-in was not reached")
            elif result.returncode != 1 or attempted:
                failures.append(f"{script} ({'empty' if not output else 'partial'} output): "
                                f"continued after mktemp failed: {attempted!r}; exit {result.returncode}")
if failures:
    raise SystemExit("\n".join("FAIL  " + failure for failure in failures))
print("PASS  five shell sandbox users stop before any command after mktemp fails")
PY
