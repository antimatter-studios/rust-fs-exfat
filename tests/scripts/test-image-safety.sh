#!/usr/bin/env bash
set -euo pipefail
bash scripts/test-image-safety.sh > /dev/null
python3 - <<'PY'
from pathlib import Path
import os
import plistlib
import re
import subprocess
import tempfile

source = Path('scripts/test-oracle.sh').read_text()
functions = {}
for name in ('cleanup', 'accept_receipt', 'detach_receipt'):
    match = re.search(r'(?ms)^' + name + r'\(\) \{\n.*?^\}\n', source)
    if not match:
        raise SystemExit('image-safety: missing receipt-scoped retry function: ' + name)
    functions[name] = match.group()

mutations = {
    'unbounded-retry': source.replace('[ "$attempt" -lt 5 ]', '[ "$attempt" -gt 0 ]'),
    'forced-detach': source.replace('hdiutil detach "$device"', 'hdiutil detach -force "$device"'),
    'foreign-retry': source.replace('hdiutil detach "$device"', 'hdiutil detach "$foreign"'),
    'lost-failure': source.replace('return "$result"', 'return 0'),
    'lost-evidence': source.replace('if [ "$result" -eq 0 ]; then', 'if true; then'),
    'early-release': source.replace('    local attempt=0', "    device=''\n    local attempt=0"),
    'unowned-retry': source.replace("attached=0\n", "attached=0\ndetach_receipt\n", 1),
    'masked-retry': source.replace('    detach_receipt\n', '    detach_receipt || true\n', 1),
    'enumeration': source + '\n/usr/bin/hdiutil info\n',
    'holder-enumeration': source.replace('/usr/sbin/lsof -nP +c 0 -- "$device" "$raw"', '/usr/sbin/lsof -nP +c 0'),
    'foreign-holder-node': source.replace('/usr/sbin/lsof -nP +c 0 -- "$device" "$raw"', '/usr/sbin/lsof -nP +c 0 -- /dev/not-owned'),
    'foreign-raw-node': source.replace('local raw="/dev/r${device#/dev/}"', 'local raw="/dev/not-owned"'),
}

with tempfile.TemporaryDirectory(prefix='image-safety-176-') as temporary:
    root = Path(temporary)
    scale = Path('tests/oracle/scale.rs').read_text()
    image = Path('tests/scale_support/mod.rs').read_text()
    rust_mutations = {
        'forced-retry': scale.replace('.args(["detach", device])', '.args(["detach", "-force", device])'),
        'foreign-detach': scale.replace('.args(["detach", device])', '.args(["detach", "/dev/not-owned"])'),
        'foreign-image': scale.replace('command.arg(&image.path)', 'command.arg("/tmp/foreign.img")'),
        'stale-receipt': scale.replace('plistlib.loads(sys.stdin.buffer.read())', 'plistlib.load(open("/tmp/stale.plist", "rb"))'),
        'unbounded-window': scale.replace('&& std::time::Instant::now() < deadline', '&& true'),
        'false-detached-state': scale.replace('"attached"', '"detached"'),
        'forced-recommendation': scale.replace('detach {device}', 'detach -force {device}'),
        'extra-device-command': scale + '\nfn unowned() { Command::new("hdiutil").args(["detach", "/dev/not-owned"]).output().unwrap(); }\n',
        'nonfresh-image': image.replace('.create_new(true)', '.create(true)'),
        'foreign-image-binding': image.replace('directory.join("generated.img")', 'PathBuf::from("/tmp/foreign.img")'),
        'lost-image-evidence': image.replace('.is_ok_and(|state| state == "attached")', '.is_ok_and(|_| false)'),
    }
    for name, contents in rust_mutations.items():
        assert contents not in (scale, image), 'ineffective Rust mutation: ' + name
        mutated = root / (name + '.rs')
        mutated.write_text(contents)
        checked = subprocess.run(['bash', 'scripts/check-image-safety.sh', str(mutated)], capture_output=True)
        assert checked.returncode != 0, 'unsafe Rust mutation accepted: ' + name

    for name, contents in mutations.items():
        assert contents != source, 'ineffective mutation: ' + name
        mutated = root / (name + '.sh')
        mutated.write_text(contents)
        checked = subprocess.run(['bash', 'scripts/check-image-safety.sh', str(mutated)], capture_output=True)
        assert checked.returncode != 0, 'unsafe mutation accepted: ' + name

    for mode, expected, attempts, retained in (
        ('busy-once', 0, 2, False),
        ('busy-four', 0, 5, False),
        ('exhausted', 1, 6, True),
        ('permanent', 1, 10, True),
        ('prior-failure', 23, 1, True),
        ('invalid-receipt', 1, 0, True),
    ):
        directory = root / mode
        directory.mkdir()
        receipt = directory / 'attach.plist'
        receipt.write_bytes(plistlib.dumps({'system-entities': [{'dev-entry': '/dev/disk999999'}]}))
        if mode == 'invalid-receipt':
            receipt.write_text('invalid receipt')
        (directory / 'image.dmg').write_bytes(b'private test image')
        harness = '''set -euo pipefail
directory="$1"
mode="$2"
attempt_log="$3"
device=''
attached=0
count=0
hdiutil() {
    [ "$#" -eq 2 ] && [ "$1" = detach ] && [ "$2" = /dev/disk999999 ] || exit 97
    count=$((count + 1))
    printf '%s\\n' "$count" >> "$attempt_log"
    case "$mode" in
        busy-once) [ "$count" -gt 1 ] ;;
        busy-four) [ "$count" -gt 4 ] ;;
        exhausted) [ "$count" -gt 5 ] ;;
        permanent) return 16 ;;
        *) return 0 ;;
    esac
}
sleep() { [ "$#" -eq 1 ] && [ "$1" = 1 ] || exit 98; }
lsof() { return 1; }
'''
        harness += ''.join(functions.values()).replace('/usr/bin/hdiutil', 'hdiutil').replace('/usr/sbin/lsof', 'lsof')
        harness += '''trap cleanup EXIT
accept_receipt
if [ "$mode" = prior-failure ]; then exit 23; fi
detach_receipt
[ -z "$device" ] && [ "$attached" -eq 0 ]
printf 'continued\\n' > "$directory/continued"
'''
        environment = dict(os.environ, EXFAT_TEST_DENY_DEVICE_PREFIX='')
        attempt_log = root / (mode + '.attempts')
        run = subprocess.run(['bash', '-c', harness, 'retry-test', str(directory), mode, str(attempt_log)],
                             capture_output=True, text=True, env=environment)
        assert run.returncode == expected, (mode, run.returncode, run.stderr)
        actual = len(attempt_log.read_text().splitlines()) if attempt_log.exists() else 0
        assert actual == attempts, (mode, actual, attempts)
        assert directory.exists() == retained, (mode, 'evidence retention')
        if retained:
            assert receipt.exists() and (directory / 'image.dmg').exists(), mode
            assert not (directory / 'continued').exists(), mode
            assert 'retained' in run.stderr, (mode, run.stderr)
        if mode in ('exhausted', 'permanent'):
            assert 'detach retry exhausted' in run.stderr, (mode, run.stderr)
print('image-safety: bounded receipt retry and retained failure evidence passed')
PY
