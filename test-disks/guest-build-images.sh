#!/usr/bin/env bash
#
# guest-build-images.sh <output-dir> <image>... — GUEST side: runs as root
# inside the fs-linux-test-harness VM (Debian), started by
# test-disks/build-fixtures.sh through `vm.sh run`.
#
# Every fixture is formatted by exfatprogs' mkfs.exfat, then LOOP-MOUNTED
# and populated through the Linux kernel's exfat driver, which no host tool
# can do and macOS cannot do on Linux's terms. The tools (exfatprogs,
# util-linux, python3) are installed by scripts/vm-setup.sh.
#
# For each image <name>.img the output directory gets:
#
#   <name>.img            the image
#   <name>.manifest       how it was made: recipe, geometry, mkfs command,
#                         exfatprogs and kernel versions
#   <name>.listing.tsv    the kernel's own listing (fixture-tool.py listing):
#                         every path, size, mode, mtime, atime and SHA-256,
#                         captured from a read-only mount after the build.
#                         For a damaged copy it is what must still be there.
#   <name>.damage.tsv     (damaged copies) every byte range the damage
#                         changed, with its SHA-256 before and after
#   <name>.deleted.tsv    (deleted recipe) the kernel's listing of the files
#                         deleted, taken just before they were
#
# Images are built on the guest's own disk (/var/tmp) and copied to the
# output directory only once complete and unmounted, so a failure half way
# never leaves a truncated image where the host picks fixtures up.
#
# DETERMINISM. The content, the names, the sizes and every timestamp set
# with touch are fixed. What still differs between two builds: the volume
# serial number (mkfs.exfat 1.2.0 has no option to set it), the times the
# kernel stamps on entries the recipe does not touch, and so the checksums
# over them. Two builds are identical in structure and content, not byte
# for byte; the listing is captured from each build, never reused.
set -euo pipefail

[ $# -ge 2 ] || { echo "usage: guest-build-images.sh <output-dir> <image>..." >&2; exit 2; }
OUTPUT_DIR="$1"
shift
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOL="python3 $HERE/fixture-tool.py"

WORK="$(mktemp -d /var/tmp/exfat-fixtures.XXXXXX)"
MNT="$WORK/mnt"
mkdir -p "$MNT" "$OUTPUT_DIR"
LOOP=""
cleanup() {
    mountpoint -q "$MNT" && umount "$MNT" || true
    [ -z "$LOOP" ] || losetup -d "$LOOP" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

# The guest's clock and zone decide the times the kernel stamps; UTC makes
# the timestamps' offset fields zero.
export TZ=UTC
EXFATPROGS="$( { mkfs.exfat -V 2>&1 || true; } | sed -n 's/^exfatprogs version : //p' | head -1)"
KERNEL="$(uname -r)"

# format <image> <size> <sector> <cluster> <label>
# mkfs.exfat takes its sector size from the device, so a 4096-byte sector
# volume is formatted through a loop device with that logical block size.
format() {
    local img="$1" size="$2" sector="$3" cluster="$4" label="$5"
    rm -f "$WORK/$img"
    truncate -s "$size" "$WORK/$img"
    LOOP="$(losetup -f --show -b "$sector" "$WORK/$img")"
    mkfs.exfat -c "$cluster" -L "$label" "$LOOP" >/dev/null
    losetup -d "$LOOP"
    LOOP=""
    cat > "$WORK/$img.manifest" <<EOF
image: $img
size: $size
sector-size: $sector
cluster-size: $cluster
label: $label
mkfs: losetup -b $sector; mkfs.exfat -c $cluster -L $label <loop device>
exfatprogs: $EXFATPROGS
kernel: $KERNEL
built: $(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
}

# The kernel writes everything: a loop mount with fmask=0 and dmask=0, so
# the mode bits it reports say only whether the read-only attribute is set.
mount_rw() { mount -t exfat -o loop,noatime,fmask=0,dmask=0,iocharset=utf8 "$WORK/$1" "$MNT"; }
mount_ro() { mount -t exfat -o loop,ro,fmask=0,dmask=0,iocharset=utf8 "$WORK/$1" "$MNT"; }

# finish <image> <recipe> — unmount, take the kernel's listing from a fresh
# read-only mount, check it with fsck.exfat, and copy everything out.
finish() {
    local img="$1" recipe="$2"
    sync
    umount "$MNT"
    echo "recipe: $recipe" >> "$WORK/$img.manifest"
    mount_ro "$img"
    $TOOL listing "$MNT" "$WORK/$img.listing.tsv"
    umount "$MNT"
    fsck.exfat -n "$WORK/$img" >/dev/null || {
        echo "[guest] fsck.exfat -n refuses the freshly built $img" >&2
        exit 1
    }
    publish "$img"
}

publish() {
    local img="$1" extra
    cp --sparse=always "$WORK/$img" "$OUTPUT_DIR/$img.partial"
    for extra in manifest listing.tsv damage.tsv deleted.tsv; do
        [ ! -e "$WORK/$img.$extra" ] || cp "$WORK/$img.$extra" "$OUTPUT_DIR/$img.$extra"
    done
    mv -f "$OUTPUT_DIR/$img.partial" "$OUTPUT_DIR/$img"
    echo "[guest] built $img"
}

# THE TREE: what most of the tests read. Small and large files, sizes either
# side of sector and cluster boundaries, a file extended by truncate and one
# written past a gap, a deep tree, long and non-ASCII names, a read-only
# file, and timestamps set at the edges exFAT can record.
populate_tree() {
    local m="$MNT" i path
    printf 'hello from exfat\n' > "$m/hello.txt"
    : > "$m/empty.bin"
    for size in 1 511 512 513 4095 4096 4097 65535 65536 65537 1048577; do
        $TOOL data "size-$size" "$size" "$m/size-$size.bin"
    done
    $TOOL data large 8388609 "$m/large.bin"
    $TOOL data extended 1000 "$m/extended.bin"
    truncate -s 3145728 "$m/extended.bin"
    $TOOL data gap 4096 "$WORK/gap.part"
    dd if="$WORK/gap.part" of="$m/gap.bin" bs=4096 seek=512 conv=notrunc status=none
    mkdir -p "$m/subdir/nested"
    printf 'nested\n' > "$m/subdir/nested.txt"
    printf 'deeper\n' > "$m/subdir/nested/deeper.txt"
    path="$m"
    for i in $(seq -w 1 30); do path="$path/d$i"; done
    mkdir -p "$path"
    printf 'thirty levels down\n' > "$path/leaf.txt"
    printf 'a name of 255 characters\n' > "$m/$(printf 'n%.0s' $(seq 1 251)).txt"
    printf 'non-ASCII\n' > "$m/Ünïcødé-名前-ファイル.txt"
    printf 'astral plane\n' > "$m/emoji-😀-𝄞.txt"
    printf 'mixed case\n' > "$m/MixedCase.TXT"
    printf 'spaces and dots\n' > "$m/ spaces . and . dots .txt"
    mkdir "$m/Ελληνικά"
    printf 'greek\n' > "$m/Ελληνικά/αρχείο.txt"
    printf 'read only\n' > "$m/read-only.txt"
    chmod a-w "$m/read-only.txt"
    printf 'times\n' > "$m/times-1980.txt"
    touch -m -d '1980-01-01 00:00:00' "$m/times-1980.txt"
    touch -a -d '1980-01-01 00:00:02' "$m/times-1980.txt"
    printf 'times\n' > "$m/times-centiseconds.txt"
    touch -m -d '2001-02-03 04:05:06.78' "$m/times-centiseconds.txt"
    touch -a -d '2001-02-03 04:05:08' "$m/times-centiseconds.txt"
    printf 'times\n' > "$m/times-2038.txt"
    touch -m -d '2038-01-19 03:14:09' "$m/times-2038.txt"
    printf 'times\n' > "$m/times-2107.txt"
    touch -m -d '2107-12-31 23:59:58' "$m/times-2107.txt"
    touch -m -d '1999-12-31 23:59:58' "$m/subdir"
}

build_tree() {
    local img="$1" size="$2" sector="$3" cluster="$4"
    format "$img" "$size" "$sector" "$cluster" TREE
    mount_rw "$img"
    populate_tree
    finish "$img" tree
}

build_tree_s512_c4k() { build_tree exfat-tree-s512-c4k.img 64M 512 4K; }
build_tree_s512_c64k() { build_tree exfat-tree-s512-c64k.img 64M 512 64K; }
build_tree_s4096_c128k() { build_tree exfat-tree-s4096-c128k.img 128M 4096 128K; }

build_empty_s512_c4k() {
    local img=exfat-empty-s512-c4k.img
    format "$img" 32M 512 4K EMPTY
    mount_rw "$img"
    finish "$img" empty
}

# The largest cluster exFAT allows, on 4096-byte sectors.
build_small_s4096_c32m() {
    local img=exfat-small-s4096-c32m.img
    format "$img" 512M 4096 32M BIGCLUSTERS
    mount_rw "$img"
    printf 'one cluster of 32 MiB\n' > "$MNT/small.txt"
    $TOOL data big-cluster 33554433 "$MNT/two-clusters.bin"
    mkdir "$MNT/dir"
    printf 'in a directory\n' > "$MNT/dir/inside.txt"
    finish "$img" small
}

# FRAGMENTED: twelve files in three directories, appended to a cluster at a
# time in turn, with entries added to the directories between rounds, so
# the files' chains and the directories' own clusters interleave.
build_fragmented_s512_c4k() {
    local img=exfat-fragmented-s512-c4k.img round file dir
    format "$img" 64M 512 4K FRAGMENTED
    mount_rw "$img"
    mkdir "$MNT/a" "$MNT/b" "$MNT/c"
    for round in $(seq 1 24); do
        for file in $(seq 1 12); do
            case $((file % 3)) in 0) dir=a ;; 1) dir=b ;; *) dir=c ;; esac
            $TOOL data "frag-$file-$round" 4096 "$WORK/chunk"
            cat "$WORK/chunk" >> "$MNT/$dir/fragment-$file.bin"
        done
        for dir in a b c; do
            printf '%s\n' "$round" > "$MNT/$dir/entry-$round-with-a-name-long-enough-for-three-name-entries-$dir.txt"
        done
        sync
    done
    finish "$img" fragmented
}

# LARGEDIR: one directory holding thirty thousand entries.
build_largedir_s512_c32k() {
    local img=exfat-largedir-s512-c32k.img
    format "$img" 64M 512 32K LARGEDIR
    mount_rw "$img"
    $TOOL bigdir "$MNT/big" 30000
    printf 'beside the big directory\n' > "$MNT/beside.txt"
    finish "$img" largedir
}

# DELETED: contiguous files written one at a time, then half of them
# deleted through the kernel, with nothing written after. The deleted
# files' listing is taken before they go.
build_deleted_s512_c4k() {
    local img=exfat-deleted-s512-c4k.img i
    format "$img" 32M 512 4K DELETED
    mount_rw "$img"
    mkdir "$MNT/gone" "$MNT/kept"
    for i in $(seq 1 8); do
        $TOOL data "deleted-$i" $((i * 5000)) "$MNT/gone/deleted-$i.bin"
        $TOOL data "kept-$i" $((i * 5000)) "$MNT/kept/kept-$i.bin"
    done
    sync
    $TOOL listing "$MNT/gone" "$WORK/$img.deleted.tsv" gone/
    rm "$MNT"/gone/deleted-*.bin
    finish "$img" deleted
}

# FULL: written until the kernel says the volume is full. The last file holds
# whatever the kernel accepted before ENOSPC.
build_full_s512_c4k() {
    local img=exfat-full-s512-c4k.img i
    format "$img" 8M 512 4K FULL
    mount_rw "$img"
    $TOOL data full-chunk 1048576 "$WORK/chunk"
    for i in $(seq 1 20); do
        if ! cp "$WORK/chunk" "$MNT/fill-$i.bin" 2>/dev/null; then
            sync
            break
        fi
    done
    df -B1 --output=avail "$MNT" | tail -1 | grep -qE '^ *[0-9]{1,5}$' || {
        echo "[guest] $img did not fill up" >&2
        exit 1
    }
    finish "$img" full
}

# DAMAGED COPIES of a built image: fixture-tool.py changes the copy and
# records every byte it changed. The listing is what must still be there:
# the origin's listing, except where the damage changes a file's content,
# when it is the kernel's listing of the damaged copy itself.
damaged() {
    local origin="$1" img="$2" kind="$3" offset=""
    [ -e "$WORK/$origin" ] || "build_$(echo "${origin%.img}" | sed 's/^exfat-//; s/-/_/g')"
    cp --sparse=always "$WORK/$origin" "$WORK/$img"
    sed "s/^image: .*/image: $img/" "$WORK/$origin.manifest" > "$WORK/$img.manifest"
    printf 'origin: %s\ndamage: %s\n' "$origin" "$kind" >> "$WORK/$img.manifest"
    if [ "$kind" = data ]; then
        mount_ro "$img"
        offset="$($TOOL fibmap "$MNT/large.bin")"
        umount "$MNT"
    fi
    $TOOL damage "$WORK/$img" "$kind" "$WORK/$img.damage.tsv" ${offset:+"$offset"}
    if [ "$kind" = data ]; then
        mount_ro "$img"
        $TOOL listing "$MNT" "$WORK/$img.listing.tsv"
        umount "$MNT"
    else
        cp "$WORK/$origin.listing.tsv" "$WORK/$img.listing.tsv"
    fi
    publish "$img"
}

build_tree_s512_c4k_boot() { damaged exfat-tree-s512-c4k.img exfat-tree-s512-c4k-boot.img boot; }
build_tree_s512_c4k_bitmap() { damaged exfat-tree-s512-c4k.img exfat-tree-s512-c4k-bitmap.img bitmap; }
build_tree_s512_c4k_upcase() { damaged exfat-tree-s512-c4k.img exfat-tree-s512-c4k-upcase.img upcase; }
build_tree_s512_c4k_data() { damaged exfat-tree-s512-c4k.img exfat-tree-s512-c4k-data.img data; }
build_fragmented_s512_c4k_boot() { damaged exfat-fragmented-s512-c4k.img exfat-fragmented-s512-c4k-boot.img boot; }

modprobe exfat
for target in "$@"; do
    "build_$target"
done
