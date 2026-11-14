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
//! Fresh single-FAT images at both sector geometries also exercise both
//! write directions, with snapshots after each operation family. Writer
//! trees have no attribute sampling. All four supported flags and the
//! label are checked through relan's tools. Its FUSE interface has no birth
//! time (GNU stat reports `-` for every path), no creation-time/UTC-offset
//! setter and no low-level allocation API; allocations are exercised through
//! fragmented writes, growth, shrink and removal in the reverse direction.
//! Fractional utime requests are truncated to whole seconds by this pin.
//! Linux 6.1 exposes only the ValidDataLength prefix after truncate growth;
//! a dedicated test pins its exact size/hash view at both geometries. The
//! writer comparisons here require relan's complete DataLength, independently
//! of that kernel divergence.
//! TexFAT is not independently verified by these single-FAT tests (#154).
//!
//! relan/exfat is GPL: built and run in the guest as an oracle, never linked
//! and never read (AGENTS.md).

mod allocation_support;
#[path = "fixture_support/mod.rs"]
mod fixture_support;
#[path = "guest_support/mod.rs"]
mod guest_support;
#[path = "guest_support/relan_write.rs"]
mod relan_write;
#[path = "walk_support/mod.rs"]
mod walk_support;
#[path = "write_support/created.rs"]
mod write_tree;

use fixture_support::{fixture, images, manifest, strict_listing, Listing};
use fs_exfat::interface::open_path;
use guest_support::write_image::WriteImage;
use std::path::Path;
use walk_support::{kernel_walk, relan_fsck, relan_walk};

fn metadata_differences<Device: fs_core::BlockRead>(
    volume: &fs_exfat::volume::Volume<Device>,
    ours: &Listing,
    relan: &walk_support::RelanWalk,
) -> Vec<String> {
    let mut found = Vec::new();
    for path in ours.keys() {
        let entry = volume.lookup(&format!("/{path}")).unwrap();
        let expected = entry.entry().unwrap().attributes();
        match relan.attributes.get(path) {
            None => found.push(format!("{path}: missing relan attribute evidence")),
            Some(actual) if *actual != expected => found.push(format!(
                "{path}: attributes are {expected:#04x} to this crate, {actual:#04x} to relan/exfat"
            )),
            Some(_) => {}
        }
    }
    for path in relan
        .attributes
        .keys()
        .filter(|path| !ours.contains_key(*path))
    {
        found.push(format!("{path}: unexpected relan attribute evidence"));
    }
    let label = volume.metadata().label.as_deref().unwrap_or("");
    if relan.label != label {
        found.push(format!(
            "label is {label:?} to this crate, {:?} to relan/exfat",
            relan.label
        ));
    }
    if relan.birth_unavailable != ours.len() {
        found.push(format!(
            "birth-time capability evidence covers {} of {} paths",
            relan.birth_unavailable,
            ours.len()
        ));
    }
    found
}

fn assert_written(path: &Path, phase: &str) -> Listing {
    let (status, output) = relan_fsck(path);
    assert_eq!(
        status, 0,
        "{phase}: relan checker refused the clean image: {output}"
    );
    let volume = open_path(path, 0).unwrap();
    let ours = strict_listing(&volume);
    let relan = walk_support::relan_walk_all(path);
    let mut expected = ours.clone();
    for row in expected.values_mut() {
        row.modified_ms = as_relan_reports(row.modified_ms);
        row.accessed_ms = as_relan_reports(row.accessed_ms);
    }
    let mut observed = relan.listing.clone();
    // FUSE modes do not expose ReadOnly. exfatattrib does, for every path.
    for (path, row) in &mut observed {
        row.read_only = *relan
            .read_only
            .get(path)
            .expect("every path has attribute evidence");
    }
    let mut found = fixture_support::differences(
        "this crate with relan's documented timestamp precision",
        &expected,
        "relan/exfat",
        &observed,
        true,
    );
    found.extend(metadata_differences(&volume, &ours, &relan));
    assert!(
        found.is_empty(),
        "{} disagreement(s):\n{}",
        found.len(),
        found.join("\n")
    );
    println!("[third vm] {phase}: all {} paths/hashes/flags and label agree; FUSE birth times unavailable at all {} paths", ours.len(), relan.birth_unavailable);
    ours
}

#[test]
fn relan_checks_the_tree_after_this_crates_allocation_and_free_cycles() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = WriteImage::new("third", sector, cluster);
        let before = strict_listing(&open_path(&image.path, 0).unwrap());
        allocation_support::allocate_and_free(&image.path);
        image.fsck("third");
        let after = assert_written(
            &image.path,
            &format!("crate allocation s{sector}/c{cluster}"),
        );
        assert_eq!(before, after);
    }
}

#[test]
fn the_kernels_truncate_growth_view_is_exactly_the_valid_prefix() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = WriteImage::new("third", sector, cluster);
        let mut writer = fs_exfat::write::open_path(&image.path, 0).unwrap();
        writer.create("/grown-empty").unwrap();
        writer.write_file("/grown-gap", b"head").unwrap();
        writer.pwrite("/grown-gap", 300000, b"tail").unwrap();
        let before = strict_listing(writer.reader());
        writer.truncate("/grown-empty", 50001).unwrap();
        writer.truncate("/grown-gap", 350001).unwrap();
        writer.unmount().unwrap();
        image.fsck("third");
        let phase = format!("kernel ValidDataLength divergence s{sector}/c{cluster}");
        let full = assert_written(&image.path, &phase);
        let kernel = kernel_walk(&image.path);
        let volume = open_path(&image.path, 0).unwrap();
        let mut expected_kernel = full.clone();
        for (path, length) in [("grown-empty", 50001), ("grown-gap", 350001)] {
            let found = volume.lookup(&format!("/{path}")).unwrap();
            let stream = found.entry().unwrap().stream();
            assert_eq!(stream.data_length(), length);
            assert_eq!(stream.valid_data_length(), before[path].size);
            let row = expected_kernel.get_mut(path).unwrap();
            row.size = before[path].size;
            row.sha256 = before[path].sha256.clone();
            println!("[third vm] evidenced kernel divergence {path} s{sector}/c{cluster}: kernel size/hash cover ValidDataLength {}; this crate and relan cover DataLength {length}", row.size);
        }
        fixture_support::assert_same(
            &phase,
            "the exact valid-prefix model (update this evidence if the kernel changes)",
            &expected_kernel,
            "the kernel",
            &kernel,
            true,
        );
    }
}

#[test]
fn relan_reads_every_kind_of_write_this_crate_makes() {
    use fs_exfat::{
        timestamp::Timestamp,
        write::{self, SetTimes},
    };
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = WriteImage::new("third", sector, cluster);
        let seed = strict_listing(&open_path(&image.path, 0).unwrap())["keep/sentinel.bin"].clone();
        write_tree::files_and_directories(&image.path);
        let mut writer = write::open_path(&image.path, 0).unwrap();
        for name in ["remove-empty", "empty-source", "empty-target"] {
            writer.mkdir(&format!("/Written/{name}")).unwrap();
        }
        writer.unmount().unwrap();
        image.fsck("third");
        let created = assert_written(
            &image.path,
            &format!("crate create/write s{sector}/c{cluster}"),
        );
        assert_eq!(created["keep/sentinel.bin"], seed);

        let mut writer = write::open_path(&image.path, 0).unwrap();
        let free = writer.free_clusters();
        writer
            .truncate("/Written/Deep/Er/large.bin", 100001)
            .unwrap();
        writer.truncate("/Written/fragment-a.bin", 7000).unwrap();
        writer.truncate("/Written/gap.bin", 350001).unwrap();
        writer.truncate("/Written/empty", 50001).unwrap();
        writer.truncate("/Written/replaced.bin", 0).unwrap();
        writer.unlink("/Written/fragment-b.bin").unwrap();
        writer
            .unlink("/Written/Ünïcödé 目录/日本語のファイル名.txt")
            .unwrap();
        writer
            .unlink("/Written/Many/entry 010 with a name of some length.dat")
            .unwrap();
        writer.rmdir("/Written/remove-empty").unwrap();
        assert!(writer.free_clusters() > free);
        writer.unmount().unwrap();
        image.fsck("third");
        let truncated = assert_written(
            &image.path,
            &format!("crate truncate/remove s{sector}/c{cluster}"),
        );
        assert_eq!(truncated["keep/sentinel.bin"], seed);
        assert_eq!(truncated["Written/empty"].size, 50001);
        assert_eq!(truncated["Written/replaced.bin"].size, 0);
        assert!(!truncated.contains_key("Written/fragment-b.bin"));
        assert!(!truncated.contains_key("Written/remove-empty"));

        let mut writer = write::open_path(&image.path, 0).unwrap();
        for (old, new) in [
            ("/Written/fragment-a.bin", "/Written/FRAGMENT-A.BIN"),
            ("/Written/ascii.txt", "/Written/replaced.bin"),
            ("/Written/empty-source", "/Written/empty-target"),
            ("/Written/Deep", "/keep/Moved by crate"),
            ("/Written/Ünïcödé 目录", "/Written/Ünïcödé 目录 renamed"),
        ] {
            writer.rename(old, new).unwrap();
        }
        let time = |ms, offset| Timestamp::from_unix_milliseconds(ms, offset).unwrap();
        writer
            .set_times(
                "/Written/dated.txt",
                SetTimes {
                    created: Some(time(1_111_111_111_110, 600)),
                    modified: Some(time(1_222_222_222_220, -210)),
                    accessed: Some(time(1_333_333_334_000, 0)),
                },
            )
            .unwrap();
        for (path, flags) in [
            ("/Written/dated.txt", 0x27),
            ("/Written/empty", 0),
            ("/keep/Moved by crate", 0x06),
            ("/Written/replaced.bin", 0x22),
        ] {
            writer.set_attributes(path, flags).unwrap();
        }
        writer.set_label("CRATE-Δ").unwrap();
        writer.unmount().unwrap();
        image.fsck("third");
        let renamed = assert_written(
            &image.path,
            &format!("crate rename/times/flags/label s{sector}/c{cluster}"),
        );
        assert_eq!(renamed["keep/sentinel.bin"], seed);
        assert_eq!(
            renamed["Written/replaced.bin"].sha256,
            created["Written/ascii.txt"].sha256
        );
        assert!(!renamed.contains_key("Written/Deep"));
        assert!(!renamed.contains_key("Written/empty-source"));
        let mut writer = write::open_path(&image.path, 0).unwrap();
        writer.set_label("").unwrap();
        writer.unmount().unwrap();
        image.fsck("third");
        assert_written(
            &image.path,
            &format!("crate empty label s{sector}/c{cluster}"),
        );
    }
}

#[test]
fn this_crate_reads_every_supported_kind_of_write_relan_makes() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = WriteImage::new("third", sector, cluster);
        let mut allocated_at_create = 0;
        for checkpoint in relan_write::produce(&image.path, sector, cluster) {
            let phase = format!("relan {} s{sector}/c{cluster}", checkpoint.phase);
            let observed = assert_written(&checkpoint.path, &phase);
            let expected = checkpoint.expected;
            assert_eq!(
                observed.keys().collect::<Vec<_>>(),
                expected.entries.keys().collect::<Vec<_>>(),
                "{phase}: complete path set"
            );
            let volume = open_path(&checkpoint.path, 0).unwrap();
            assert_eq!(
                volume.metadata().label.as_deref().unwrap_or(""),
                expected.label,
                "{phase}: label"
            );
            for (path, model) in expected.entries {
                let row = &observed[&path];
                assert_eq!(row.directory, model.directory, "{phase}: {path}: type");
                if let Some(size) = model.size {
                    assert_eq!(row.size, size, "{phase}: {path}: size");
                }
                assert_eq!(
                    row.sha256, model.sha256,
                    "{phase}: {path}: complete file SHA-256"
                );
                assert_eq!(
                    row.modified_ms, expected.modified_ms,
                    "{phase}: {path}: mtime"
                );
                assert_eq!(
                    row.accessed_ms, expected.accessed_ms,
                    "{phase}: {path}: atime"
                );
                let entry = volume.lookup(&format!("/{path}")).unwrap();
                assert_eq!(
                    entry.entry().unwrap().attributes(),
                    model.attributes,
                    "{phase}: {path}: flags"
                );
            }
            if checkpoint.phase == "create" {
                allocated_at_create = volume.bitmap().allocated_count();
                let many = volume.lookup("/Relan/Many").unwrap();
                assert!(many.entry().unwrap().stream().data_length() > u64::from(cluster));
                for name in ["a", "b"] {
                    let found = volume
                        .lookup(&format!("/Relan/fragment-{name}.bin"))
                        .unwrap();
                    assert!(
                        !found.entry().unwrap().stream().no_fat_chain(),
                        "{phase}: fragmented {name}"
                    );
                }
            } else if checkpoint.phase == "truncate" {
                assert!(
                    volume.bitmap().allocated_count() < allocated_at_create,
                    "{phase}: released clusters"
                );
            }
        }
    }
}

#[test]
fn full_writer_metadata_comparison_refuses_missing_or_wrong_evidence() {
    let image = WriteImage::new("third", 512, 4096);
    let volume = open_path(&image.path, 0).unwrap();
    let ours = strict_listing(&volume);
    let mut relan = walk_support::relan_walk_all(&image.path);
    assert!(metadata_differences(&volume, &ours, &relan).is_empty());
    let flags = relan.attributes.remove("keep/sentinel.bin").unwrap();
    let found = metadata_differences(&volume, &ours, &relan);
    assert_eq!(
        found,
        ["keep/sentinel.bin: missing relan attribute evidence"]
    );
    relan
        .attributes
        .insert("keep/sentinel.bin".into(), flags ^ 2);
    let found = metadata_differences(&volume, &ours, &relan);
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("keep/sentinel.bin: attributes"));
    relan.attributes.insert("keep/sentinel.bin".into(), flags);
    relan.attributes.insert("ghost".into(), 0);
    assert_eq!(
        metadata_differences(&volume, &ours, &relan),
        ["ghost: unexpected relan attribute evidence"]
    );
    relan.attributes.remove("ghost");
    relan.label = "wrong".into();
    assert!(metadata_differences(&volume, &ours, &relan)[0].starts_with("label is"));
    relan.label = "ALLOCATION".into();
    relan.birth_unavailable = 0;
    assert!(metadata_differences(&volume, &ours, &relan)[0]
        .starts_with("birth-time capability evidence"));
}

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
