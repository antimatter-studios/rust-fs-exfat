#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo"
binary="${CARGO_TARGET_DIR:-target}/debug/rust-fs-exfat"
[[ -x "$binary" ]] || {
    echo 'sleuthkit probe: missing CLI; run chore build with the same CARGO_TARGET_DIR' >&2
    exit 1
}
mkdir -p tmp
scratch="$(mktemp -d "$repo/tmp/sleuthkit-178.XXXXXX")"
echo "sleuthkit probe: receipts and fresh images in $scratch"
python3 - "$scratch" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
payload = bytes(index % 251 for index in range(12288))
(root / 'expected.bin').write_bytes(payload)
(root / 'first.bin').write_bytes(payload[:4096])
(root / 'rest.bin').write_bytes(payload[4096:])
(root / 'blocker.bin').write_bytes(b'B' * 4096)
(root / 'wrong.bin').write_bytes(payload[:4096] + b'B' * 4096)
PY
for fats in 1 2; do
    image="$scratch/fats$fats.img"
    "$binary" mkfs --size 64M --fats "$fats" --cluster-size 4096 --sector-size 512 --serial 17800001 "$image" > "$scratch/fats$fats.json"
    cat "$scratch/fats$fats.json"
    "$binary" fs "$image" write /chain.bin < "$scratch/first.bin"
    "$binary" fs "$image" write /blocker.bin < "$scratch/blocker.bin"
    "$binary" fs "$image" write --at 4096 /chain.bin < "$scratch/rest.bin"
    "$binary" fs "$image" stat /chain.bin > "$scratch/fats$fats-stat.json"
done
python3 - "$scratch" <<'PY'
from pathlib import Path
import json
import shutil
import struct
import sys

root = Path(sys.argv[1])
layout = json.loads((root / 'fats2.json').read_text())
entry = json.loads((root / 'fats2-stat.json').read_text())
assert (layout['fat_offset'], layout['fat_length'], layout['cluster_heap_offset']) == (24, 128, 280)
assert entry['first_cluster'] == 7 and not entry['no_fat_chain'] and entry['size'] == 12288
with (root / 'fats2.img').open('rb') as image:
    for copy in (0, 1):
        image.seek((24 + copy * 128) * 512 + 7 * 4)
        assert struct.unpack('<I', image.read(4))[0] == 9
        image.seek((24 + copy * 128) * 512 + 9 * 4)
        assert struct.unpack('<I', image.read(4))[0] == 10
for active in (0, 1):
    mirrored = root / f'mirrored-active{active}.img'
    shutil.copyfile(root / 'fats2.img', mirrored)
    with mirrored.open('r+b') as image:
        for offset in (106, 12 * 512 + 106):
            image.seek(offset)
            image.write(bytes([active, 0]))
    inactive = 1 - active
    for field in ('fat', 'bitmap'):
        path = root / f'{field}-disagree-active{active}.img'
        shutil.copyfile(mirrored, path)
        with path.open('r+b') as image:
            if field == 'fat':
                image.seek((24 + inactive * 128) * 512 + 7 * 4)
                image.write(struct.pack('<I', 8))
            else:
                for cluster in (7, 9, 10):
                    position = 280 * 512 + inactive * 4096 + (cluster - 2) // 8
                    image.seek(position)
                    value = image.read(1)[0] & ~(1 << ((cluster - 2) % 8))
                    image.seek(position)
                    image.write(bytes([value]))
PY
for image in "$scratch"/*.img; do
    "$binary" fs "$image" cat /chain.bin | cmp "$scratch/expected.bin" -
done
cat > "$scratch/guest.rs" <<'RS'
#[path = "../../tests/guest_support/mod.rs"]
mod guest_support;

fn main() {
    let script = std::fs::read_to_string(std::env::args().nth(1).expect("script path"))
        .expect("read guest script");
    let result = guest_support::guest("sleuthkit", "pinned ActiveFat capability probe", &script);
    print!("{}", result.stdout);
    eprint!("{}", result.stderr);
    std::process::exit(result.status);
}
RS
CARGO_MANIFEST_DIR="$repo" rustc --edition=2021 "$scratch/guest.rs" -o "$scratch/guest"
printf 'root=%q\n' "/repo/${scratch#"$repo"/}" > "$scratch/probe.sh"
cat >> "$scratch/probe.sh" <<'SH'
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq sleuthkit=4.11.1+dfsg-1+b1 >/dev/null
for package in sleuthkit libtsk19; do
    version="$(dpkg-query -W -f='${Version}' "$package")"
    [ "$version" = 4.11.1+dfsg-1+b1 ] || { echo "unexpected $package version: $version" >&2; exit 1; }
    dpkg-query -W -f='${Package} ${Version} ${Architecture}\n' "$package"
done
for tool in fls icat blkstat timeout cmp sha256sum; do
    command -v "$tool" >/dev/null || { echo "missing guest tool: $tool; provision the harness guest" >&2; exit 1; }
done
[ "$(fls -V)" = 'The Sleuth Kit ver 4.11.1' ]
scratch="$(mktemp -d /var/tmp/sleuthkit-178.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT
cp "$root/expected.bin" "$root/wrong.bin" "$scratch/"
sha256sum "$scratch/expected.bin" "$scratch/wrong.bin"
for name in fats1 mirrored-active0 mirrored-active1 fat-disagree-active0 fat-disagree-active1 bitmap-disagree-active0 bitmap-disagree-active1; do
    image="$scratch/$name.img"
    cp --sparse=always "$root/$name.img" "$image"
    timeout -k 2 30 fls -f exfat "$image" > "$scratch/listing"
    printf '\n%s: fls exit 0\n' "$name"
    cat "$scratch/listing"
    inode="$(awk '$NF == "chain.bin" {sub(/:$/, "", $2); print $2}' "$scratch/listing")"
    [ -n "$inode" ] || { echo 'missing chain.bin inode' >&2; exit 1; }
    timeout -k 2 30 icat -f exfat "$image" "$inode" > "$scratch/actual.bin"
    printf 'icat exit 0, bytes=%s\n' "$(wc -c < "$scratch/actual.bin")"
    sha256sum "$scratch/actual.bin"
    if [ "$name" = fat-disagree-active1 ]; then
        cmp "$scratch/wrong.bin" "$scratch/actual.bin"
        echo 'WRONG CONTENT: inactive First FAT was followed'
    else
        cmp "$scratch/expected.bin" "$scratch/actual.bin"
        echo 'content matches'
    fi
    sector=320
    [ "$name" != fats1 ] || sector=184
    timeout -k 2 30 blkstat -f exfat "$image" "$sector" > "$scratch/allocation"
    cat "$scratch/allocation"
    if [ "$name" = bitmap-disagree-active1 ]; then
        grep -Fxq 'Not Allocated' "$scratch/allocation"
        echo 'WRONG ALLOCATION: inactive First Allocation Bitmap was used'
    else
        grep -Fxq 'Allocated' "$scratch/allocation"
    fi
done
echo 'VERDICT: Sleuth Kit 4.11.1 is unsuitable as an ActiveFat-aware oracle; negative evidence reproduced'
SH
"$repo/../fs-linux-test-harness/scripts/vm.sh" session "$scratch/guest" "$scratch/probe.sh"
