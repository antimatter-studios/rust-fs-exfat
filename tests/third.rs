//! `chore test:third`: CROSS-VALIDATION against a third implementation.
//!
//! relan/exfat (its FUSE driver, `exfatattrib` and `exfatfsck`), built in the
//! guest at a pinned commit, shares no code with this crate or with the Linux
//! kernel. Every undamaged fixture is read three ways, by relan, the kernel
//! and this crate's strict reader, and they must agree on every path, type,
//! size, SHA-256 and read-only attribute, and on the times as each one
//! records them. A disagreement names the path, the field and all three
//! values.
//!
//! Where relan differs from the other two, the kernel and this crate agree
//! and relan is the one off the specification. Those ways are written down
//! here, each exactly, and nothing else is let through:
//!
//! - its FUSE driver reports times in whole seconds, dropping the 10 ms
//!   increment fields (section 7.4.6);
//! - it counts 2100 as a leap year, so a time from 2100-03-01 on reads a day
//!   late (section 7.4.8's years run to 2107, and 2100 is not a leap year);
//! - its FUSE modes do not carry the read-only attribute, so that attribute
//!   is read with its `exfatattrib` instead;
//! - its `exfatfsck` does not check the Up-case Table's checksum.
//!
//! Once write support lands (#133), the same walks check the other
//! direction: relan reads what this crate wrote, and this crate reads what
//! relan wrote.
//!
//! relan/exfat is GPL: built and run in the guest as an oracle, never linked
//! and never read (AGENTS.md).

#[path = "fixture_support/mod.rs"]
mod fixture_support;
#[path = "guest_support/mod.rs"]
mod guest_support;
#[path = "walk_support/mod.rs"]
mod walk_support;

use fixture_support::{fixture, images, manifest, strict_listing, Listing};
use fs_exfat::interface::open_path;
use walk_support::{kernel_walk, relan_fsck, relan_walk};

/// 2100-03-01T00:00:00Z, the first moment relan/exfat reads a day late.
const RELAN_LEAP_ERROR_FROM_MS: i64 = 4_107_542_400_000;
const DAY_MS: i64 = 86_400_000;

/// The time relan/exfat reports for a time the kernel and this crate agree
/// on, given the two ways it is known to differ.
fn as_relan_reports(ms: i64) -> i64 {
    let whole_seconds = ms.div_euclid(1000) * 1000;
    if ms >= RELAN_LEAP_ERROR_FROM_MS {
        whole_seconds + DAY_MS
    } else {
        whole_seconds
    }
}

fn three_way(
    image: &str,
    kernel: &Listing,
    ours: &Listing,
    relan: &walk_support::RelanWalk,
) -> Vec<String> {
    let mut out = Vec::new();
    let paths: std::collections::BTreeSet<&String> = kernel
        .keys()
        .chain(ours.keys())
        .chain(relan.listing.keys())
        .collect();
    for path in paths {
        let (Some(k), Some(o), Some(r)) =
            (kernel.get(path), ours.get(path), relan.listing.get(path))
        else {
            out.push(format!(
                "{image}: {path}: listed by the kernel {}, this crate {}, relan/exfat {}",
                kernel.contains_key(path),
                ours.contains_key(path),
                relan.listing.contains_key(path)
            ));
            continue;
        };
        let mut field =
            |name: &str, kernel: String, ours: String, relan: String, relan_ok: bool| {
                if kernel != ours || !relan_ok {
                    out.push(format!(
                        "{image}: {path}: {name} is {kernel} to the kernel, {ours} to this crate, \
                     {relan} to relan/exfat"
                    ));
                }
            };
        field(
            "type",
            k.directory.to_string(),
            o.directory.to_string(),
            r.directory.to_string(),
            r.directory == k.directory,
        );
        field(
            "size",
            k.size.to_string(),
            o.size.to_string(),
            r.size.to_string(),
            r.size == k.size,
        );
        field(
            "sha256",
            format!("{:?}", k.sha256),
            format!("{:?}", o.sha256),
            format!("{:?}", r.sha256),
            r.sha256 == k.sha256,
        );
        field(
            "mtime ms",
            k.modified_ms.to_string(),
            o.modified_ms.to_string(),
            r.modified_ms.to_string(),
            r.modified_ms == as_relan_reports(k.modified_ms),
        );
        field(
            "atime ms",
            k.accessed_ms.to_string(),
            o.accessed_ms.to_string(),
            r.accessed_ms.to_string(),
            r.accessed_ms == as_relan_reports(k.accessed_ms),
        );
        if let Some(relan_ro) = relan.read_only.get(path) {
            field(
                "read-only",
                k.read_only.to_string(),
                o.read_only.to_string(),
                relan_ro.to_string(),
                *relan_ro == k.read_only,
            );
        }
    }
    out
}

#[test]
fn relan_the_kernel_and_this_crate_read_every_fixture_alike() {
    let mut entries = 0;
    let mut attributes = 0;
    for image in images() {
        if manifest(&image).damaged() {
            continue;
        }
        let path = fixture(&image);
        let kernel = kernel_walk(&path);
        let ours = strict_listing(&open_path(&path, 0).unwrap());
        let relan = relan_walk(&path);
        let found = three_way(&image, &kernel, &ours, &relan);
        assert!(
            found.is_empty(),
            "{} disagreement(s):\n  {}",
            found.len(),
            found
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        );
        entries += kernel.len();
        attributes += relan.read_only.len();
    }
    println!(
        "[third vm] relan/exfat, the kernel and this crate agreed three ways on {entries} entries \
         and {attributes} read-only attributes"
    );
}

#[test]
fn relans_known_differences_are_exactly_the_ones_allowed() {
    // Whole seconds: 2001-02-03 04:05:06.78 reads as ...:06.
    assert_eq!(as_relan_reports(981_173_106_780), 981_173_106_000);
    // A day late from 2100-03-01: 2107-12-31 23:59:58 reads as 2108-01-01.
    assert_eq!(as_relan_reports(4_354_819_198_000), 4_354_905_598_000);
    // Neither applies before 2100-03-01 to a whole second.
    assert_eq!(as_relan_reports(4_107_542_399_000), 4_107_542_399_000);
    // A disagreement in any other field is reported, naming all three values.
    let image = "exfat-tree-s512-c4k.img";
    let path = fixture(image);
    let kernel = kernel_walk(&path);
    let ours = strict_listing(&open_path(&path, 0).unwrap());
    let mut relan = relan_walk(&path);
    relan.listing.get_mut("hello.txt").unwrap().size += 1;
    let found = three_way(image, &kernel, &ours, &relan);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0]
            .contains("hello.txt: size is 17 to the kernel, 17 to this crate, 18 to relan/exfat"),
        "{}",
        found[0]
    );
}

#[test]
fn exfatfsck_accepts_every_undamaged_fixture_and_refuses_structural_damage() {
    let mut checked = 0;
    for image in images() {
        let manifest = manifest(&image);
        let (status, output) = relan_fsck(&fixture(&image));
        match manifest.find("damage") {
            // A file's content is not a structure a checker can see.
            None | Some("data") => assert_eq!(
                status, 0,
                "{image}: relan/exfat's exfatfsck -n refuses a volume whose structures are intact:\n{output}"
            ),
            // exfatfsck does not check the Up-case Table's checksum (section
            // 7.2.2): it passes the damaged table, which fsck.exfat and the
            // strict reader refuse. Asserted, so a change in relan shows.
            Some("upcase") => assert_eq!(
                status, 0,
                "{image}: relan/exfat's exfatfsck -n now refuses a damaged up-case table; \
                 move this copy to the refused ones:\n{output}"
            ),
            Some("boot" | "bitmap") => assert_ne!(
                status, 0,
                "{image}: relan/exfat's exfatfsck -n accepts structural damage:\n{output}"
            ),
            Some(other) => panic!("{image}: no expectation for damage {other}"),
        }
        checked += 1;
    }
    println!("[third vm] exfatfsck -n judged {checked} fixtures");
}
