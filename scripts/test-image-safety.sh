#!/usr/bin/env bash
set -euo pipefail
mkdir -p tmp
directory="$(mktemp -d "$PWD/tmp/image-safety.XXXXXX")"
trap 'rm -r "$directory"' EXIT
printf 'os.fsync(output.fileno())\n' > "$directory/allowed.py"
bash scripts/check-image-safety.sh "$directory/allowed.py"
printf 'os.sync()\n' > "$directory/global.py"
printf 'sync\n' > "$directory/global.sh"
printf '/bin/sync\n' > "$directory/absolute.sh"
for source_file in "$directory/global.py" "$directory/global.sh" "$directory/absolute.sh"; do
    if bash scripts/check-image-safety.sh "$source_file" > "$directory/rejected.log" 2>&1; then
        echo 'image-safety: a global flush was incorrectly accepted' >&2
        exit 1
    fi
done
printf '/usr/sbin/diskutil list\n' > "$directory/foreign-device.sh"
if bash scripts/check-image-safety.sh "$directory/foreign-device.sh" > "$directory/rejected.log" 2>&1; then
    echo 'image-safety: forbidden diskutil enumeration was incorrectly accepted' >&2
    exit 1
fi
bash scripts/check-image-safety.sh
python3 - "$directory" <<'PY'
from pathlib import Path
import sys
directory = Path(sys.argv[1])
source = Path('scripts/test-oracle.sh').read_text()
query = '/usr/sbin/diskutil info -plist "$device"'
variants = {
    'other-verb': source.replace(query, '/usr/sbin/diskutil repairVolume "$device"'),
    'other-device': source.replace(query, '/usr/sbin/diskutil info -plist /dev/not-owned'),
    'missing-plist': source.replace(query, '/usr/sbin/diskutil info "$device"'),
    'reassignment': source.replace('    accept_receipt\n', '    accept_receipt\n    device=/dev/not-owned\n', 1),
    'foreign-receipt': source.replace('python3 - "$directory/attach.plist"', 'python3 - "$directory/foreign.plist"'),
    'receipt-reuse': source.replace('    /usr/bin/hdiutil attach -blocksize "$sector" -nomount -nobrowse -plist "$image" > "$directory/attach.plist"', '    true'),
    'foreign-image': source.replace('-plist "$image"', '-plist "$foreign_image"'),
    'foreign-detach': source.replace('/usr/bin/hdiutil detach "$device"', '/usr/bin/hdiutil detach /dev/not-owned'),
    'unowned-query': source.replace("device=''\n", "device=''\n" + query + '\n', 1),
    'loop-rebinding': source.replace('    accept_receipt\n', '    accept_receipt\n    for image in "$foreign"; do :; done\n', 1),
    'heredoc-device': source.replace('import struct, sys', 'import struct, sys\nopen("/dev/not-owned", "rb")', 1),
    'purge': source + '\npurge\n',
    'process-kill': source + '\npkill -f hdiutil\n',
    'libc-flush': source + '\nlibc::sync();\n',
}
for name, contents in variants.items():
    if contents == source:
        raise SystemExit('image-safety: mutation did not change the source: ' + name)
    (directory / (name + '.bad')).write_text(contents)
PY
for source_file in "$directory/other-verb.bad" "$directory/other-device.bad" "$directory/missing-plist.bad" "$directory/reassignment.bad" "$directory/foreign-receipt.bad" "$directory/receipt-reuse.bad" "$directory/foreign-image.bad" "$directory/foreign-detach.bad" "$directory/unowned-query.bad" "$directory/loop-rebinding.bad" "$directory/heredoc-device.bad" "$directory/purge.bad" "$directory/process-kill.bad" "$directory/libc-flush.bad"; do
    if bash scripts/check-image-safety.sh "$source_file" > "$directory/rejected.log" 2>&1; then
        echo "image-safety: unsafe mutation was incorrectly accepted: $source_file" >&2
        exit 1
    fi
done
if bash scripts/check-image-safety.sh "$directory/absent" > "$directory/rejected.log" 2>&1; then
    echo 'image-safety: an absent source was incorrectly accepted' >&2
    exit 1
fi
ln -s "$directory/allowed.py" "$directory/link.py"
if bash scripts/check-image-safety.sh "$directory/link.py" > "$directory/rejected.log" 2>&1; then
    echo 'image-safety: a symlink was incorrectly accepted' >&2
    exit 1
fi
echo 'image-safety: positive and negative guard checks passed'
