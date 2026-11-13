//! Walks of a whole volume by the other two exFAT implementations, in the
//! harness guest: the Linux kernel's driver (a read-only loop mount) and
//! relan/exfat's FUSE driver, built at a pinned commit by
//! `scripts/vm-setup.sh`. Each takes any image under the repository, so the
//! write direction (#133) can hand them a volume this crate wrote.
//!
//! Both are GPL. They run in the guest as oracles; nothing here links or
//! reads them.
//!
//! A test crate using this module declares `fixture_support` and
//! `guest_support` beside it.

#![allow(dead_code)]

use crate::fixture_support::{parse_listing, Listing};
use crate::guest_support::{guest, guest_path, quote, repo, stage_image};
use std::collections::BTreeMap;
use std::path::Path;

/// The relan/exfat commit scripts/vm-setup.sh builds. A guest built from
/// another is refused, so an oracle never moves without this file moving.
pub const RELAN_PIN: &str = "1264da3d59840434c657e2199d9c8248355940a6";
const RELAN: &str = "/opt/relan-exfat/sbin";

/// How many entries of a volume relan's `exfatattrib` is asked about, in
/// path order. Each call opens the image afresh: the 30,000-entry directory
/// took minutes asked about a thousand. Every fixture but that one has fewer.
const ATTRIBUTE_SAMPLE: usize = 200;

fn tool() -> String {
    quote(&guest_path(&repo().join("test-disks/fixture-tool.py")))
}

/// The kernel's listing of `image`, loop-mounted read-only in the guest now.
pub fn kernel_walk(image: &Path) -> Listing {
    let script = format!(
        "{stage}mkdir \"$scratch/mnt\"\n\
         trap 'umount \"$scratch/mnt\" 2>/dev/null || true; rm -rf \"$scratch\"' EXIT\n\
         modprobe exfat\n\
         mount -t exfat -o loop,ro,fmask=0,dmask=0,iocharset=utf8 \"$image\" \"$scratch/mnt\"\n\
         python3 {tool} listing \"$scratch/mnt\" \"$scratch/listing.tsv\" >/dev/null\n\
         umount \"$scratch/mnt\"\n\
         cat \"$scratch/listing.tsv\"\n",
        stage = stage_image(image),
        tool = tool(),
    );
    let name = image.file_name().unwrap().to_string_lossy();
    let out = guest(
        "kernel",
        &format!("mount -o loop,ro and walk {name}"),
        &script,
    )
    .success("the kernel's walk");
    let listing = parse_listing(&out.stdout, &format!("the kernel's walk of {name}"));
    println!(
        "[kernel vm] the kernel listed {} entries of {name}",
        listing.len()
    );
    listing
}

pub fn kernel_birth_times(image: &Path) -> BTreeMap<String, i64> {
    let script = format!(
        "{stage}mkdir \"$scratch/mnt\"\n\
         trap 'umount \"$scratch/mnt\" 2>/dev/null || true; rm -rf \"$scratch\"' EXIT\n\
         modprobe exfat\n\
         mount -t exfat -o loop,ro,fmask=0,dmask=0,iocharset=utf8 \"$image\" \"$scratch/mnt\"\n\
         python3 {tool} \"$scratch/mnt\" > \"$scratch/birth-times.json\"\n\
         umount \"$scratch/mnt\"\n\
         cat \"$scratch/birth-times.json\"\n",
        stage = stage_image(image),
        tool = quote(&guest_path(
            &repo().join("tests/write_support/birth-times.py")
        )),
    );
    let name = image.file_name().unwrap().to_string_lossy();
    let out = guest(
        "kernel",
        &format!("mount -o loop,ro and stat birth times of {name}"),
        &script,
    )
    .success("the kernel's birth times");
    serde_json::from_str(&out.stdout).expect("the kernel's birth times as JSON")
}

/// relan/exfat's reading of a volume: its FUSE driver's listing, and the
/// read-only attribute its `exfatattrib` reports for up to
/// [`ATTRIBUTE_SAMPLE`] entries (the FUSE driver's modes do not carry it).
pub struct RelanWalk {
    pub listing: Listing,
    pub read_only: BTreeMap<String, bool>,
}

/// relan/exfat's FUSE driver mounts the image read-only (through a
/// read-only loop device: as root it asks for a block device) and the walk
/// runs over that mount.
pub fn relan_walk(image: &Path) -> RelanWalk {
    let script = format!(
        "{stage}mkdir \"$scratch/mnt\"\n\
         loop=''\n\
         trap 'umount \"$scratch/mnt\" 2>/dev/null || true; [ -z \"$loop\" ] || losetup -d \"$loop\"; rm -rf \"$scratch\"' EXIT\n\
         [ \"$(cat /opt/relan-exfat/relan-exfat.pin)\" = {pin} ] || {{ echo 'relan/exfat in the guest is not at the pinned commit; chore vm:provision rebuilds it' >&2; exit 1; }}\n\
         modprobe fuse\n\
         loop=\"$(losetup -r -f --show \"$image\")\"\n\
         {relan}/mount.exfat-fuse -o ro \"$loop\" \"$scratch/mnt\" 2>/dev/null\n\
         python3 {tool} listing \"$scratch/mnt\" \"$scratch/listing.tsv\" >/dev/null\n\
         umount \"$scratch/mnt\"\n\
         cat \"$scratch/listing.tsv\"\n\
         echo '--- attributes'\n\
         awk -F '\\t' -v n={sample} '!/^#/ && ++seen <= n {{ print $7 }}' \"$scratch/listing.tsv\" |\n\
         while IFS= read -r path; do\n\
             ro=\"$({relan}/exfatattrib -d \"$loop\" \"/$path\" | sed -n 's/^ *Read-only: //p')\"\n\
             printf '%s\\t%s\\n' \"$ro\" \"$path\"\n\
         done\n",
        stage = stage_image(image),
        pin = quote(RELAN_PIN),
        relan = RELAN,
        tool = tool(),
        sample = ATTRIBUTE_SAMPLE,
    );
    let name = image.file_name().unwrap().to_string_lossy();
    let out = guest(
        "third",
        &format!("relan/exfat mount.exfat-fuse -o ro, walk and exfatattrib {name}"),
        &script,
    )
    .success("relan/exfat's walk");
    let (listing, attributes) = out
        .stdout
        .split_once("--- attributes\n")
        .expect("the attribute section");
    let listing = parse_listing(listing, &format!("relan/exfat's walk of {name}"));
    let read_only = attributes
        .lines()
        .map(|line| {
            let (flag, path) = line.split_once('\t').expect("flag and path");
            assert!(
                matches!(flag, "yes" | "no"),
                "exfatattrib said {flag:?} for {path}"
            );
            (path.to_owned(), flag == "yes")
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(read_only.len(), listing.len().min(ATTRIBUTE_SAMPLE));
    println!(
        "[third vm] relan/exfat listed {} entries of {name} and gave the attributes of {}",
        listing.len(),
        read_only.len()
    );
    RelanWalk { listing, read_only }
}

/// relan/exfat's `exfatfsck -n` on an image: the checker's status and output.
pub fn relan_fsck(image: &Path) -> (i32, String) {
    let script = format!(
        "{stage}set +e\n{relan}/exfatfsck -n \"$image\" 2>&1\necho \"status: $?\"\n",
        stage = stage_image(image),
        relan = RELAN,
    );
    let name = image.file_name().unwrap().to_string_lossy();
    let out = guest(
        "third",
        &format!("relan/exfat exfatfsck -n {name}"),
        &script,
    )
    .success("relan/exfat's checker");
    let status = out
        .stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("status: "))
        .expect("the checker's status")
        .parse()
        .unwrap();
    println!("[third vm] exfatfsck -n {name}: status {status}");
    (status, out.stdout)
}
