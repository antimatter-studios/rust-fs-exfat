#![allow(dead_code)]

//! #186, the kernel direction: the Linux kernel makes the equivalent of each
//! kind of write in `kinds.rs` on a read-write loop mount in the harness
//! guest, and after each one the volume is unmounted and saved as a
//! checkpoint named for the kind, with the kernel's own listing and birth
//! times taken just before. This crate's strict reader must then see what
//! the kernel listed, and what the kind was meant to leave.
//!
//! The kernel has no interface for two kinds. Allocation without an entry is
//! not a file operation at all. The volume label has no kernel interface on
//! the guest's kernel, so exfatprogs' `exfatlabel` sets and clears it on the
//! unmounted loop device instead; the matrix names it as that tool's write.
//!
//! A test crate using this module declares `fixture_support`,
//! `guest_support` and `walk_support` beside it.

use crate::fixture_support::{parse_listing, Listing};
use crate::guest_support::write_image::WriteImage;
use crate::guest_support::{guest, guest_path, quote, repo};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The kernel-direction kinds in order, each with who makes it.
pub const KERNEL_KINDS: [(&str, &str); 14] = [
    ("creation", "kernel"),
    ("truncate-grow", "kernel"),
    ("truncate-shrink", "kernel"),
    ("truncate-zero", "kernel"),
    ("truncate-fragmented", "kernel"),
    ("unlink", "kernel"),
    ("rmdir", "kernel"),
    ("rename-case-only", "kernel"),
    ("rename-cross-directory", "kernel"),
    ("rename-replacement", "kernel"),
    ("timestamps", "kernel"),
    ("attributes", "kernel"),
    ("label-set", "exfatlabel"),
    ("label-clear", "exfatlabel"),
];

/// The label `label-set` writes, as `kinds::LABEL`.
pub const LABEL: &str = "Écrit 2026é";

/// The bytes the guest writes for `seed`; the same formula on both sides.
pub fn content(seed: u64, length: u64) -> Vec<u8> {
    (0..length)
        .map(|index| ((index * 7 + seed * 31) & 255) as u8)
        .collect()
}

/// One kernel-made checkpoint: the image, and the kernel's listing and
/// birth times of it while still mounted.
pub struct Checkpoint {
    pub kind: &'static str,
    pub image: PathBuf,
    pub listing: Listing,
    pub birth: BTreeMap<String, i64>,
}

/// Make every kind with the kernel on `image`, in one guest call, and return
/// the checkpoints in order.
pub fn kernel_writes(image: &WriteImage, sector: u32, cluster: u32) -> Vec<Checkpoint> {
    let destinations: Vec<PathBuf> = KERNEL_KINDS
        .iter()
        .map(|(kind, _)| image.checkpoint_path(&format!("kernel-{kind}")))
        .collect();
    let script = format!(
        r#"for tool in losetup mount umount python3 truncate exfatlabel blockdev cp mv rm rmdir chmod mktemp modprobe; do
    command -v "$tool" >/dev/null || {{ echo "missing $tool; run 'chore vm:provision'" >&2; exit 1; }}
done
scratch="$(mktemp -d /var/tmp/exfat-kernel-write.XXXXXX)"
loop=''
cleanup() {{
    status=$?
    trap - EXIT
    if mountpoint -q "$scratch/mnt"; then
        umount "$scratch/mnt" || exit 1
    fi
    [ -z "$loop" ] || losetup -d "$loop" || exit 1
    rm -rf "$scratch"
    exit "$status"
}}
trap cleanup EXIT
image="$scratch/image"
cp --sparse=always {source} "$image"
loop="$(losetup -f --show -b {sector} "$image")"
modprobe exfat
m="$scratch/mnt"
k="$m/Kinds"
mkdir "$m"
up() {{
    mount -t exfat -o noatime,fmask=0,dmask=0,iocharset=utf8 "$loop" "$m"
}}
# The kernel's listing and birth times of the mounted volume, then the
# unmounted image saved as the checkpoint named for the kind.
checkpoint() {{
    python3 {tool} listing "$m" "$scratch/listing.tsv" >/dev/null
    python3 {birth} "$m" > "$scratch/birth.json"
    umount "$m"
    blockdev --flushbufs "$loop"
    cp --sparse=always "$image" "$2"
    echo "=== $1"
    cat "$scratch/listing.tsv"
    echo '=== birth'
    cat "$scratch/birth.json"
    echo
}}
fill() {{
    python3 -c 'import sys; open(sys.argv[1], "ab").write(bytes(((i * 7 + int(sys.argv[2]) * 31) & 255) for i in range(int(sys.argv[3]))))' "$@"
}}
c={cluster}
up
mkdir "$k" "$k/From" "$k/From/Moved directory" "$k/To" "$k/Empty" "$k/Nested" "$k/Nested/Inner" "$k/Replaced directory" "$k/Replacing directory"
fill "$k/grow.bin" 1 $((c / 2 + 7))
fill "$k/shrink.bin" 2 $((c * 5 + 11))
fill "$k/zero.bin" 3 $((c * 3))
fill "$k/unlinked.bin" 4 $((c * 2 + 1))
: > "$k/unlinked empty"
printf 'über' > "$k/Ünïcödé gone.txt"
printf 'case only\n' > "$k/MixedCase.txt"
printf 'moved across\n' > "$k/From/moved.txt"
fill "$k/From/Moved directory/inside.bin" 5 999
fill "$k/replacing.bin" 6 $((c + 3))
fill "$k/replaced.bin" 7 $((c * 4))
: > "$k/dated.txt"
printf 'flags\n' > "$k/flags.txt"
: > "$k/fragment-a.bin"
: > "$k/fragment-b.bin"
for round in 0 1 2 3 4 5 6 7; do
    fill "$k/fragment-a.bin" $((round * 2 + 10)) $((c + 1))
    fill "$k/fragment-b.bin" $((round * 2 + 11)) $((c + 1))
done
checkpoint creation {creation}
up
truncate -s $((c * 3 + 513)) "$k/grow.bin"
checkpoint truncate-grow {grow}
up
truncate -s $((c * 2 - 5)) "$k/shrink.bin"
checkpoint truncate-shrink {shrink}
up
truncate -s 0 "$k/zero.bin"
checkpoint truncate-zero {zero}
up
truncate -s $((c + 100)) "$k/fragment-a.bin"
truncate -s $((8 * (c + 1) - c * 3 - 17)) "$k/fragment-b.bin"
truncate -s $((8 * (c + 1) + c * 2)) "$k/fragment-b.bin"
checkpoint truncate-fragmented {fragmented}
up
rm "$m/KINDS/UNLINKED.BIN" "$k/unlinked empty" "$k/Ünïcödé gone.txt"
checkpoint unlink {unlink}
up
rmdir "$k/Nested/Inner" "$k/Nested" "$k/Empty"
checkpoint rmdir {rmdir}
up
# rename(2) itself: mv refuses a name that resolves to the same file.
python3 -c 'import os, sys; os.rename(sys.argv[1], sys.argv[2]); os.rename(sys.argv[3], sys.argv[4])' "$k/MixedCase.txt" "$k/MIXEDCASE.TXT" "$k/To" "$k/TO"
checkpoint rename-case-only {case}
up
mv "$k/From/moved.txt" "$k/TO/moved here.txt"
mv "$k/From/Moved directory" "$k/TO/Moved directory"
checkpoint rename-cross-directory {cross}
up
mv "$k/replacing.bin" "$k/replaced.bin"
mv -T "$k/Replacing directory" "$k/Replaced directory"
checkpoint rename-replacement {replacement}
up
python3 -c 'import os, sys
for path in sys.argv[1:3]:
    os.utime(path, ns=(1333333334000000000, 1222222222220000000))
os.utime(sys.argv[3], ns=(os.stat(sys.argv[3]).st_atime_ns, 1444444444440000000))' "$k/dated.txt" "$k/TO" "$k/grow.bin"
checkpoint timestamps {timestamps}
up
chmod a-w "$k/flags.txt"
checkpoint attributes {attributes}
exfatlabel "$loop" {label} >/dev/null
up
checkpoint label-set {label_set}
exfatlabel "$loop" '' >/dev/null
up
checkpoint label-clear {label_clear}
"#,
        source = quote(&guest_path(&image.path)),
        tool = quote(&guest_path(&repo().join("test-disks/fixture-tool.py"))),
        birth = quote(&guest_path(
            &repo().join("tests/write_support/birth-times.py")
        )),
        label = quote(LABEL),
        creation = quote(&guest_path(&destinations[0])),
        grow = quote(&guest_path(&destinations[1])),
        shrink = quote(&guest_path(&destinations[2])),
        zero = quote(&guest_path(&destinations[3])),
        fragmented = quote(&guest_path(&destinations[4])),
        unlink = quote(&guest_path(&destinations[5])),
        rmdir = quote(&guest_path(&destinations[6])),
        case = quote(&guest_path(&destinations[7])),
        cross = quote(&guest_path(&destinations[8])),
        replacement = quote(&guest_path(&destinations[9])),
        timestamps = quote(&guest_path(&destinations[10])),
        attributes = quote(&guest_path(&destinations[11])),
        label_set = quote(&guest_path(&destinations[12])),
        label_clear = quote(&guest_path(&destinations[13])),
    );
    let out = guest(
        "kernel",
        &format!("the kernel makes every kind of write on s{sector}/c{cluster}"),
        &script,
    )
    .success("the kernel's writes (run `chore vm:provision` for guest tools)");
    let mut checkpoints = Vec::new();
    for (index, section) in out
        .stdout
        .split("=== ")
        .skip(1)
        .collect::<Vec<_>>()
        .chunks(2)
        .enumerate()
    {
        let kind = KERNEL_KINDS[index].0;
        let (named, listing) = section[0].split_once('\n').unwrap();
        assert_eq!(named, kind, "the guest's checkpoints come in order");
        let birth = section[1].strip_prefix("birth\n").expect("birth section");
        checkpoints.push(Checkpoint {
            kind,
            image: destinations[index].clone(),
            listing: parse_listing(listing, &format!("the kernel's listing after {kind}")),
            birth: serde_json::from_str(birth.trim()).expect("the kernel's birth times as JSON"),
        });
    }
    assert_eq!(
        checkpoints.len(),
        KERNEL_KINDS.len(),
        "a checkpoint per kind"
    );
    checkpoints
}

fn read<Device: fs_core::BlockRead>(
    volume: &fs_exfat::volume::Volume<Device>,
    path: &str,
) -> Vec<u8> {
    let found = volume
        .lookup(path)
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    let file = volume.open_file(found.entry().unwrap()).unwrap();
    let mut bytes = vec![0; file.len() as usize];
    assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
    bytes
}

/// The bytes the guest appended to a fragment file in eight rounds.
fn fragment(first_seed: u64, cluster: u64) -> Vec<u8> {
    (0..8)
        .flat_map(|round| content(first_seed + round * 2, cluster + 1))
        .collect()
}

/// What `kind` must have left, as this crate's strict reader sees it, apart
/// from the kernel's own listing. Returns a note for the evidence line where
/// the kernel's behaviour is itself the finding.
pub fn expect<Device: fs_core::BlockRead>(
    kind: &str,
    volume: &fs_exfat::volume::Volume<Device>,
    cluster: u64,
) -> Option<String> {
    let absent = |path: &str| assert!(volume.lookup(path).is_err(), "{kind}: {path} remains");
    let entry = |path: &str| {
        volume
            .lookup(path)
            .unwrap_or_else(|error| panic!("{kind}: {path}: {error}"))
            .entry()
            .unwrap()
            .clone()
    };
    match kind {
        "creation" => {
            assert_eq!(read(volume, "/Kinds/grow.bin"), content(1, cluster / 2 + 7));
            assert_eq!(read(volume, "/Kinds/zero.bin"), content(3, cluster * 3));
            assert_eq!(read(volume, "/Kinds/Ünïcödé gone.txt"), "über".as_bytes());
            assert_eq!(read(volume, "/Kinds/fragment-a.bin"), fragment(10, cluster));
            for path in ["/Kinds/fragment-a.bin", "/Kinds/fragment-b.bin"] {
                assert!(
                    !entry(path).stream().no_fat_chain(),
                    "{path}: the kernel's alternating appends must make a FAT chain"
                );
            }
        }
        "truncate-grow" => {
            let bytes = read(volume, "/Kinds/grow.bin");
            let kept = (cluster / 2 + 7) as usize;
            assert_eq!(bytes.len() as u64, cluster * 3 + 513);
            assert_eq!(bytes[..kept], content(1, kept as u64)[..]);
            assert!(bytes[kept..].iter().all(|byte| *byte == 0));
        }
        "truncate-shrink" => assert_eq!(
            read(volume, "/Kinds/shrink.bin"),
            content(2, cluster * 5 + 11)[..(cluster * 2 - 5) as usize]
        ),
        "truncate-zero" => assert_eq!(entry("/Kinds/zero.bin").stream().data_length(), 0),
        "truncate-fragmented" => {
            assert_eq!(
                read(volume, "/Kinds/fragment-a.bin"),
                fragment(10, cluster)[..(cluster + 100) as usize]
            );
            let bytes = read(volume, "/Kinds/fragment-b.bin");
            let kept = (8 * (cluster + 1) - cluster * 3 - 17) as usize;
            assert_eq!(bytes.len() as u64, 8 * (cluster + 1) + cluster * 2);
            assert_eq!(bytes[..kept], fragment(11, cluster)[..kept]);
            assert!(bytes[kept..].iter().all(|byte| *byte == 0));
        }
        "unlink" => {
            absent("/Kinds/unlinked.bin");
            absent("/Kinds/unlinked empty");
            absent("/Kinds/Ünïcödé gone.txt");
        }
        "rmdir" => {
            absent("/Kinds/Nested");
            absent("/Kinds/Empty");
        }
        "rename-case-only" => {
            let mut names: Vec<String> = volume
                .read_directory(&entry("/Kinds"))
                .unwrap()
                .iter()
                .map(|entry| entry.name().to_owned())
                .filter(|name| {
                    name.eq_ignore_ascii_case("mixedcase.txt") || name.eq_ignore_ascii_case("to")
                })
                .collect();
            names.sort();
            // Linux resolves the new name to the same file, and rename(2) of
            // a file onto itself changes nothing: the names keep their case.
            assert_eq!(
                names,
                ["MixedCase.txt", "To"],
                "the kernel's case-only rename changed a name; replace this platform assertion"
            );
            return Some(
                "rename(2) to a case-only variant leaves the name unchanged on Linux".to_owned(),
            );
        }
        "rename-cross-directory" => {
            assert_eq!(read(volume, "/Kinds/TO/moved here.txt"), b"moved across\n");
            assert_eq!(
                read(volume, "/Kinds/TO/Moved directory/inside.bin"),
                content(5, 999)
            );
            absent("/Kinds/From/moved.txt");
            absent("/Kinds/From/Moved directory");
        }
        "rename-replacement" => {
            assert_eq!(read(volume, "/Kinds/replaced.bin"), content(6, cluster + 3));
            absent("/Kinds/replacing.bin");
            absent("/Kinds/Replacing directory");
        }
        "timestamps" => {
            for path in ["/Kinds/dated.txt", "/Kinds/TO"] {
                let found = entry(path);
                let times = found.timestamps();
                assert_eq!(
                    crate::fixture_support::milliseconds(&times.modified),
                    1_222_222_222_220,
                    "{path}"
                );
                assert_eq!(
                    crate::fixture_support::milliseconds(&times.accessed),
                    1_333_333_334_000,
                    "{path}"
                );
            }
            let found = entry("/Kinds/grow.bin");
            let times = found.timestamps();
            assert_eq!(
                crate::fixture_support::milliseconds(&times.modified),
                1_444_444_444_440
            );
        }
        "attributes" => assert_eq!(entry("/Kinds/flags.txt").attributes() & 0x01, 0x01),
        "label-set" => assert_eq!(volume.metadata().label.as_deref(), Some(LABEL)),
        "label-clear" => assert_eq!(volume.metadata().label.as_deref().unwrap_or(""), ""),
        other => panic!("no expectation for {other}"),
    }
    None
}
