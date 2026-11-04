#!/usr/bin/env bash
set -euo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'oracle: macOS is required; run chore test:oracle on macOS' >&2; exit 1; }
for tool in /usr/bin/hdiutil /sbin/newfs_exfat /sbin/fsck_exfat /usr/bin/python3; do
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
for sector in 512 4096; do
    image="$directory/reference-$sector.dmg"
    /usr/bin/hdiutil create -size 64m -layout NONE "$image"
    /usr/bin/hdiutil attach -blocksize "$sector" -nomount -nobrowse -plist "$image" > "$directory/attach.plist"
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
    /sbin/newfs_exfat -R -S "$sector" -b 4096 -v M1ORACLE "$device" | tee "$directory/format-$sector.txt"
    /sbin/fsck_exfat -n "$device"
    /usr/bin/hdiutil detach "$device"
    device=''
    attached=0
done
EXFAT_ORACLE_DIR="$directory" cargo test --locked --test macos_boot -- --nocapture
