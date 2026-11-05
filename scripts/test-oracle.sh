#!/usr/bin/env bash
set -euo pipefail
bash scripts/check-image-safety.sh "$0"
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
(root / 'contiguous.bin').write_bytes(bytes(range(256)) * 4096)
(root / 'fragmented.bin').write_bytes(b'A' * 4096)
(root / 'blocker.bin').write_bytes(b'B' * 4096)
with (root / 'fragmented.bin').open('ab') as output:
    output.write(b'C' * 12288)
(root / 'valid-tail.bin').write_bytes(b'U' * 12288)
(root / 'timestamp.bin').write_bytes(b'timestamp oracle\n')
os.utime(root / 'timestamp.bin', ns=(1700000000120000000, 1700000001120000000))
PY
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
    /usr/bin/python3 - "$image" <<'PY'
import struct, sys
with open(sys.argv[1], 'r+b') as image:
    boot = image.read(512)
    sector = 1 << boot[108]
    cluster_size = sector << boot[109]
    heap = struct.unpack_from('<I', boot, 88)[0] * sector
    fat = struct.unpack_from('<I', boot, 80)[0] * sector
    current = struct.unpack_from('<I', boot, 96)[0]
    seen, positions, directory = set(), [], bytearray()
    while current != 0xffffffff:
        if current in seen:
            raise SystemExit('oracle: generated root chain cycle')
        seen.add(current)
        position = heap + (current - 2) * cluster_size
        positions.append(position)
        image.seek(position)
        directory.extend(image.read(cluster_size))
        image.seek(fat + current * 4)
        current = struct.unpack('<I', image.read(4))[0]
    index = 0
    found = False
    while index < len(directory) and directory[index] != 0:
        if directory[index] != 0x85:
            index += 32
            continue
        length = (directory[index + 1] + 1) * 32
        entry = directory[index:index + length]
        name_bytes = b''.join(entry[start + 2:start + 32] for start in range(64, length, 32) if entry[start] == 0xc1)
        name = name_bytes[:entry[35] * 2].decode('utf-16le')
        if name == 'valid-tail.bin':
            struct.pack_into('<Q', entry, 40, 4096)
            checksum = 0
            for position, byte in enumerate(entry):
                if position not in (2, 3):
                    checksum = (((checksum >> 1) | (checksum << 15)) + byte) & 0xffff
            struct.pack_into('<H', entry, 2, checksum)
            directory[index:index + length] = entry
            found = True
            break
        index += length
    if not found:
        raise SystemExit('oracle: populate valid-tail.bin before changing its valid length')
    for index, position in enumerate(positions):
        image.seek(position)
        image.write(directory[index * cluster_size:(index + 1) * cluster_size])
PY
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
    /usr/bin/python3 - "$directory/mount-$sector" "$directory/listing-$sector.txt" "$directory/files-$sector.txt" <<'PY'
from pathlib import Path
import hashlib, os, sys
root = Path(sys.argv[1])
records, contents = [], []
for current, directories, files in os.walk(root):
    for name in directories + files:
        item = Path(current) / name
        if item.is_symlink():
            raise SystemExit('oracle: unexpected symlink on generated image')
        kind = 'd' if item.is_dir() else 'f'
        length = 0 if kind == 'd' else item.stat().st_size
        records.append(f'{kind}\t{length}\t{item.relative_to(root)}\n')
        if kind == 'f':
            metadata = item.stat()
            digest = hashlib.sha256(item.read_bytes()).hexdigest()
            contents.append(f'{length}\t{digest}\t{metadata.st_mtime_ns // 1000000}\t{metadata.st_atime_ns // 1000000}\t{round(metadata.st_birthtime * 1000)}\t{item.relative_to(root)}\n')
with open(sys.argv[2], 'w') as output:
    output.writelines(sorted(records))
with open(sys.argv[3], 'w') as output:
    output.writelines(sorted(contents))
PY
    /usr/bin/python3 - "$directory/mount-$sector" "$directory/lookup-$sector.txt" <<'PY'
import hashlib, os, stat, sys
queries = ['.', 'ascii.TXT', 'MIXEDCASE.txt', 'nested/A LONG FILE NAME SPANNING ENTRIES.TXT',
           'GRÜßE-😀.TXT', 'GRÜSSE-😀.TXT', 'Nested/../aScIi.TxT', 'Nested/missing/../aScIi.TxT',
           'absent.txt', 'ASCII.txt/child', 'ASCII.txt/']
with open(sys.argv[2], 'w') as output:
    for query in queries:
        item = os.path.join(sys.argv[1], query)
        try:
            metadata = os.stat(item)
            if stat.S_ISDIR(metadata.st_mode):
                value = 'directory'
            elif stat.S_ISREG(metadata.st_mode):
                with open(item, 'rb') as source:
                    digest = hashlib.sha256(source.read()).hexdigest()
                value = f'file\t{metadata.st_size}\t{digest}'
            else:
                raise SystemExit('oracle: unexpected file type in path lookup')
        except FileNotFoundError:
            value = 'missing'
        except NotADirectoryError:
            value = 'not-directory'
        output.write(query + '\t' + value + '\n')
PY
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
done
EXFAT_ORACLE_DIR="$directory" cargo test --locked --features cli --test macos_boot --test macos_metadata --test macos_directory --test macos_file --test macos_path --test macos_interfaces -- --nocapture
bash scripts/test-capi.sh "$directory/reference-512.dmg" "$directory/reference-4096.dmg"
