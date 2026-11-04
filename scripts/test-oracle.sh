#!/usr/bin/env bash
set -euo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'oracle: macOS is required; run chore test:oracle on macOS' >&2; exit 1; }
for tool in /usr/bin/hdiutil /sbin/newfs_exfat /sbin/fsck_exfat /usr/bin/python3 /usr/sbin/diskutil; do
    [ -x "$tool" ] || { echo "oracle: $tool is required by chore test:oracle" >&2; exit 1; }
done
mkdir -p tmp
directory="$(mktemp -d "$PWD/tmp/oracle.XXXXXX")"
device=''
attached=0
cleanup() {
    local result=$?
    if [ -n "$device" ]; then
        if ! /usr/bin/hdiutil detach "$device"; then
            echo "oracle: detach failed; retained receipt and image in $directory" >&2
            return 1
        fi
        attached=0
    fi
    if [ "$attached" -ne 0 ]; then
        echo "oracle: attach receipt could not be parsed; retained $directory/attach.plist" >&2
        return 1
    fi
    if [ "$result" -eq 0 ]; then
        rm -r "$directory"
    else
        echo "oracle: failure evidence retained in $directory" >&2
    fi
    return "$result"
}
trap cleanup EXIT
accept_receipt() {
    attached=1
    device="$(/usr/bin/python3 - "$directory/attach.plist" <<'PY'
import plistlib, re, sys
with open(sys.argv[1], 'rb') as source:
    receipt = plistlib.load(source)
devices = [entry['dev-entry'] for entry in receipt['system-entities']
           if re.fullmatch(r'/dev/disk[0-9]+', entry.get('dev-entry', ''))]
if len(devices) != 1:
    raise SystemExit('oracle: expected exactly one whole-image device in this attach receipt')
print(devices[0])
PY
)"
    case "${EXFAT_TEST_DENY_DEVICE_PREFIX:-}" in
        '') ;;
        *) case "$device" in "$EXFAT_TEST_DENY_DEVICE_PREFIX"*) device=''; exit 1 ;; esac ;;
    esac
}
for sector in 512 4096; do
    image="$directory/reference-$sector.dmg"
    /usr/bin/hdiutil create -size 64m -layout NONE "$image"
    /usr/bin/hdiutil attach -blocksize "$sector" -nomount -nobrowse -plist "$image" > "$directory/attach.plist"
    accept_receipt
    /sbin/newfs_exfat -R -S "$sector" -b 4096 -v M1ORACLE "$device" | tee "$directory/format-$sector.txt"
    /sbin/fsck_exfat -n "$device"
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
    /usr/bin/python3 - "$image" <<'PY'
import struct, sys
with open(sys.argv[1], 'r+b') as image:
    boot = image.read(512)
    cluster_size = 1 << (boot[108] + boot[109])
    heap = struct.unpack_from('<I', boot, 88)[0] << boot[108]
    root = struct.unpack_from('<I', boot, 96)[0]
    root_offset = heap + (root - 2) * cluster_size
    image.seek(root_offset)
    directory = image.read(cluster_size)
    position = next(index for index in range(0, cluster_size - 32, 32) if directory[index] == 0)
    entry = bytearray(32)
    entry[0] = 0xa0
    entry[6:22] = bytes(range(1, 17))
    checksum = 0
    for index, byte in enumerate(entry):
        if index not in (2, 3):
            checksum = (((checksum >> 1) | (checksum << 15)) + byte) & 0xffff
    struct.pack_into('<H', entry, 2, checksum)
    image.seek(root_offset + position)
    image.write(entry)
PY
    mkdir "$directory/mount-$sector"
    /usr/bin/hdiutil attach -blocksize "$sector" -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"
    accept_receipt
    /usr/bin/python3 - "$directory/mount-$sector" <<'PY'
from pathlib import Path
import os, sys
root = Path(sys.argv[1])
(root / '.metadata_never_index').write_bytes(b'')
(root / 'Nested').mkdir()
(root / 'Empty').mkdir()
for name in ['ASCII.txt', 'MixedCase.txt', 'grüße-😀.txt', 'Nested/a long file name spanning entries.txt']:
    (root / name).write_bytes(('contents of ' + name + '\n').encode())
for index in range(180):
    (root / f'item-{index:03}.txt').write_bytes(bytes([index]) * (index + 1))
PY
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
    /usr/bin/hdiutil attach -blocksize "$sector" -readonly -nobrowse -mountpoint "$directory/mount-$sector" -plist "$image" > "$directory/attach.plist"
    accept_receipt
    /sbin/fsck_exfat -n "$device"
    /usr/sbin/diskutil info -plist "$device" > "$directory/info-$sector.plist"
    /usr/bin/python3 - "$directory/info-$sector.plist" "$directory/driver-$sector.txt" <<'PY'
import plistlib, sys
with open(sys.argv[1], 'rb') as source:
    info = plistlib.load(source)
if not info.get('MountPoint') or any(info.get(key, True) for key in ('Writable', 'WritableMedia', 'WritableVolume')):
    raise SystemExit('oracle: driver image must be mounted read-only')
with open(sys.argv[2], 'w') as output:
    output.write(info['VolumeName'] + '\n' + info['VolumeUUID'].upper() + '\nread-only\n')
PY
    /usr/bin/python3 - "$directory/mount-$sector" "$directory/listing-$sector.txt" <<'PY'
from pathlib import Path
import os, sys
root = Path(sys.argv[1])
records = []
for current, directories, files in os.walk(root):
    for name in directories + files:
        item = Path(current) / name
        if item.is_symlink():
            raise SystemExit('oracle: unexpected symlink on generated image')
        kind = 'd' if item.is_dir() else 'f'
        length = 0 if kind == 'd' else item.stat().st_size
        records.append(f'{kind}\t{length}\t{item.relative_to(root)}\n')
with open(sys.argv[2], 'w') as output:
    output.writelines(sorted(records))
PY
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
done
EXFAT_ORACLE_DIR="$directory" cargo test --locked --test macos_boot --test macos_metadata --test macos_directory -- --nocapture
