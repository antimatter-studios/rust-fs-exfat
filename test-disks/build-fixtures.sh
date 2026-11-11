#!/usr/bin/env bash
#
# build-fixtures.sh [image...]  build the test-disks/*.img fixtures
#                               (`chore fixtures`); name some to rebuild
#                               only those, e.g. `build-fixtures.sh tree_s512_c4k`
# build-fixtures.sh --check     exit 1 naming every fixture, manifest or
#                               listing that is missing
# build-fixtures.sh --list      print every image file name, one per line
#
# THE FIXTURE LIST lives here (IMAGES below) and in chores.yml's `fixtures`
# generates:, which must name the same files.
#
# Every fixture is populated by the Linux kernel's exfat driver, so the work
# happens in the fs-linux-test-harness VM (the sibling checkout at
# ../fs-linux-test-harness, at the ref `chore siblings` pins):
# test-disks/guest-build-images.sh runs as root in the guest, writes finished
# images, manifests and kernel listings into the shared directory, and this
# script moves them into test-disks/.
#
# The VM comes down when this script exits, unless it is held, shared with
# another live session, or FLTH_KEEP_VM=1 keeps it.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# builder target -> image file, in build order. A damaged copy follows the
# image it is made from.
IMAGES="empty_s512_c4k:exfat-empty-s512-c4k.img
tree_s512_c4k:exfat-tree-s512-c4k.img
tree_s512_c64k:exfat-tree-s512-c64k.img
tree_s4096_c128k:exfat-tree-s4096-c128k.img
small_s4096_c32m:exfat-small-s4096-c32m.img
fragmented_s512_c4k:exfat-fragmented-s512-c4k.img
largedir_s512_c32k:exfat-largedir-s512-c32k.img
deleted_s512_c4k:exfat-deleted-s512-c4k.img
full_s512_c4k:exfat-full-s512-c4k.img
tree_s512_c4k_boot:exfat-tree-s512-c4k-boot.img
tree_s512_c4k_bitmap:exfat-tree-s512-c4k-bitmap.img
tree_s512_c4k_upcase:exfat-tree-s512-c4k-upcase.img
tree_s512_c4k_data:exfat-tree-s512-c4k-data.img
fragmented_s512_c4k_boot:exfat-fragmented-s512-c4k-boot.img"

if [ "${1:-}" = "--list" ]; then
    for pair in $IMAGES; do echo "${pair#*:}"; done
    exit 0
fi

if [ "${1:-}" = "--check" ]; then
    gone=""
    for pair in $IMAGES; do
        img="${pair#*:}"
        for file in "$img" "$img.manifest" "$img.listing.tsv"; do
            [ -f "$REPO/test-disks/$file" ] || gone="$gone $file"
        done
    done
    if [ -n "$gone" ]; then
        echo "fixtures missing from test-disks/:$gone" >&2
        echo "build them with 'chore fixtures' — tests never skip on a missing fixture." >&2
        exit 1
    fi
    echo "fixtures: all $(echo "$IMAGES" | wc -w | tr -d ' ') present in test-disks/"
    exit 0
fi

targets="$*"
if [ -z "$targets" ]; then
    for pair in $IMAGES; do targets="$targets ${pair%%:*}"; done
fi

HARNESS="$REPO/../fs-linux-test-harness"
VM="$HARNESS/scripts/vm.sh"

if [ ! -x "$VM" ]; then
    echo "build-fixtures: the harness is not checked out at $HARNESS." >&2
    echo "                Run 'chore siblings' first." >&2
    exit 1
fi

cd "$REPO"
# shellcheck source=/dev/null
. "$HARNESS/scripts/vm-session.sh"

share="$("$VM" share)"
out="$share/fixtures"
rm -rf "$out"
mkdir -p "$out"
cp test-disks/guest-build-images.sh test-disks/fixture-tool.py "$share/"

started=$(date +%s)
# shellcheck disable=SC2086  # the targets are words
"$VM" run "bash /share/guest-build-images.sh /share/fixtures $targets"

built=0
for img in "$out"/*.img; do
    [ -e "$img" ] || continue
    base="$(basename "$img")"
    # Checked on the host rather than taken on the guest's word: an undamaged
    # image, and every damaged one but the boot-sector copies, carries the
    # exFAT file system name at byte 3 of its Main Boot Sector.
    case "$base" in
        *-boot.img) ;;
        *)
            name="$(dd if="$img" bs=1 skip=3 count=8 2>/dev/null)"
            [ "$name" = "EXFAT   " ] || {
                echo "build-fixtures: $base has no exFAT boot sector (got '$name')" >&2
                exit 1
            }
            ;;
    esac
    [ -f "$img.manifest" ] && [ -f "$img.listing.tsv" ] || {
        echo "build-fixtures: the guest made $base without its manifest or listing" >&2
        exit 1
    }
    for file in "$img".*; do mv -f "$file" "test-disks/$(basename "$file")"; done
    mv -f "$img" "test-disks/$base"
    built=$((built + 1))
done
[ "$built" -gt 0 ] || { echo "build-fixtures: the guest produced no images" >&2; exit 1; }
echo "build-fixtures: $built image(s) in test-disks/ ($(( $(date +%s) - started ))s in the VM)"
