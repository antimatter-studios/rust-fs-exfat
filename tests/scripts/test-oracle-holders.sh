#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
from pathlib import Path
import os
import plistlib
import re
import subprocess
import tempfile

source = Path('scripts/test-oracle.sh').read_text()
functions = []
for name in ('cleanup', 'accept_receipt', 'detach_receipt'):
    match = re.search(r'(?ms)^' + name + r'\(\) \{\n.*?^\}\n', source)
    assert match, 'missing ownership function: ' + name
    functions.append(match.group())

with tempfile.TemporaryDirectory(prefix='oracle-holders-') as temporary:
    root = Path(temporary)
    for mode in ('holder', 'no-holder', 'query-error'):
        directory = root / mode
        directory.mkdir()
        receipt = directory / 'attach.plist'
        receipt.write_bytes(plistlib.dumps({'system-entities': [{'dev-entry': '/dev/disk999999'}]}))
        image = directory / 'image.dmg'
        image.write_bytes(b'own image evidence')
        harness = '''set -euo pipefail
directory="$1"
mode="$2"
device=''
attached=0
hdiutil() {
    [ "$#" -eq 2 ] && [ "$1" = detach ] && [ "$2" = /dev/disk999999 ] || exit 97
    echo ordinary >> "$directory/detaches"
    return 16
}
lsof() {
    [ "$#" -eq 6 ] && [ "$1" = -nP ] && [ "$2" = +c ] && [ "$3" = 0 ] &&
        [ "$4" = -- ] && [ "$5" = /dev/disk999999 ] && [ "$6" = /dev/rdisk999999 ] || exit 98
    echo scoped >> "$directory/queries"
    case "$mode" in
        holder) printf 'COMMAND PID USER FD TYPE NAME\\nDisk Drill 840 chris 21r CHR /dev/rdisk999999\\n'; return 0 ;;
        no-holder) return 1 ;;
        query-error) echo 'lsof unavailable' >&2; return 127 ;;
    esac
}
sleep() { [ "$#" -eq 1 ] && [ "$1" = 1 ] || exit 99; }
'''
        harness += ''.join(functions).replace('/usr/bin/hdiutil', 'hdiutil').replace('/usr/sbin/lsof', 'lsof')
        harness += '''trap cleanup EXIT
accept_receipt
detach_receipt
echo continued > "$directory/continued"
'''
        run = subprocess.run(['bash', '-c', harness, 'holder-test', str(directory), mode],
                             env=dict(os.environ, EXFAT_TEST_DENY_DEVICE_PREFIX=''), capture_output=True, text=True)
        assert run.returncode == 1, (mode, run.returncode, run.stderr)
        queries = directory / 'queries'
        assert queries.exists(), 'exhausted receipt-owned detaches must query their holders'
        assert len(queries.read_text().splitlines()) == 2, (mode, run.stderr)
        assert len((directory / 'detaches').read_text().splitlines()) == 10, mode
        assert receipt.is_file() and image.read_bytes() == b'own image evidence', mode
        assert not (directory / 'continued').exists(), mode
        assert 'retained' in run.stderr, (mode, run.stderr)
        if mode == 'holder':
            assert 'Disk Drill' in run.stderr and '840' in run.stderr, run.stderr
        else:
            assert 'could not identify' in run.stderr, run.stderr
print('oracle holders: receipt-scoped diagnostics retain detach failure and evidence')
PY
