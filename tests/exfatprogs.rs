//! `chore test:oracle`: exfatprogs, in the harness guest, judges every
//! kernel-made fixture.
//!
//! `fsck.exfat -n` is the consistency checker: it must accept every
//! undamaged image and refuse every copy whose damage is structural. It
//! cannot see a file's content, so the copy with overwritten data passes it
//! while its content differs from the original's: the gap the kernel's
//! listing and this crate's hashes close.
//!
//! `dump.exfat` reports the boot sector and the root's system entries; each
//! field must equal what this crate parses from the same image.
//!
//! exfatprogs is GPL: it runs in the guest as an oracle, never linked and
//! never read (AGENTS.md).

mod allocation_support;
#[path = "write_support/cuts.rs"]
mod cuts;
#[path = "fixture_support/mod.rs"]
mod fixture_support;
#[path = "guest_support/mod.rs"]
mod guest_support;
#[path = "support/image.rs"]
mod image_support;
#[path = "write_support/kinds.rs"]
mod kinds;
#[path = "oracle/linux_mkfs.rs"]
mod linux_mkfs;
#[path = "write_support/created.rs"]
mod write_support;

use fixture_support::{fixture, images, kernel_listing, manifest};
use fs_exfat::interface::open_path;
use guest_support::{guest, stage_image};
use std::collections::BTreeMap;

#[test]
fn fsck_exfat_accepts_the_writers_allocation_and_volume_state_changes() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = guest_support::write_image::WriteImage::new("oracle", sector, cluster);
        allocation_support::allocate_and_free(&image.path);
        image.fsck("oracle");
    }
}

#[test]
fn fsck_exfat_accepts_files_and_directories_created_by_this_crate() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = guest_support::write_image::WriteImage::new("oracle", sector, cluster);
        write_support::files_and_directories(&image.path);
        image.fsck("oracle");
    }
}

/// #185: one clean, unmounted checkpoint after each kind of write, on a
/// fresh guest-formatted image at each sector size. A refused checkpoint is
/// kept beside the image, named for the operation that produced it.
#[test]
fn fsck_exfat_accepts_a_clean_checkpoint_after_every_kind_of_write() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        let image = guest_support::write_image::WriteImage::new("oracle", sector, cluster);
        let mut done = Vec::new();
        kinds::run(&image.path, |_, kind| {
            kinds::save_checkpoint(&image.path, &image.checkpoint_path(kind));
            done.push(kind.to_owned());
        });
        let done: Vec<&str> = done.iter().map(String::as_str).collect();
        assert_eq!(done.len(), kinds::KINDS.len());
        image.fsck_checkpoints("oracle", &done);
    }
}

/// #188: the block-cut crash matrix. The write script is replayed onto a
/// fresh guest-formatted image once for every sector it writes, cut off
/// there. exfatprogs judges every partial image and repairs a copy of each
/// one it refuses; the strict reader then finds every file and directory
/// the interrupted operation was not changing exactly as it was. A cut at a
/// step boundary must be clean as it stands.
#[test]
fn every_block_cut_of_the_write_script_leaves_the_other_files_intact() {
    for (sector, cluster) in allocation_support::GEOMETRIES {
        block_cut_matrix(sector, cluster);
    }
}

fn block_cut_matrix(sector: u32, cluster: u32) {
    use guest_support::write_cuts;
    // Retaining every failing image would fill a runner; deltas.bin keeps
    // every cut, rebuildable in order from the retained fresh image.
    const RETAINED: usize = 8;
    let image = guest_support::write_image::WriteImage::with_size("oracle", sector, cluster, "8M");
    let name = format!("s{sector}/c{cluster}");
    let base = cuts::Image::open(&image.path);
    let plan = cuts::Plan::measure(&base, u64::from(sector));
    let mut previous = base.clone();
    let mut records = Vec::new();
    let mut deltas = Vec::new();
    for cut in 0..=plan.total() {
        let mut record = cuts::replay(&base, u64::from(sector), &plan, cut);
        let image = std::mem::replace(&mut record.image, base.clone());
        deltas.push((cut, cuts::delta(&previous, &image, sector as usize)));
        previous = image;
        records.push(record);
    }
    let verdicts = write_cuts::judge(
        "oracle",
        &format!("fsck.exfat on {} block cuts of {name}", deltas.len()),
        &image.path,
        image.directory(),
        sector,
        &deltas,
    );

    let mut current = base;
    let mut problems = Vec::new();
    let (mut clean, mut repaired, mut leaked) = (0, 0, 0);
    for (record, (_, delta)) in records.iter().zip(&deltas) {
        cuts::apply(&mut current, delta);
        let verdict = &verdicts[&record.cut];
        let judged = match &verdict.repair {
            None => {
                clean += 1;
                Ok(current.clone())
            }
            Some(_) if record.at_boundary() => Err(format!(
                "fsck.exfat -n refuses a clean unmount (status {})",
                verdict.check
            )),
            Some((repair, after, _)) if !matches!(repair, 0 | 1) || *after != 0 => Err(format!(
                "fsck.exfat -y cannot repair it (status {repair}, then -n status {after})"
            )),
            Some((_, _, fixes)) => {
                repaired += 1;
                let mut copy = current.clone();
                cuts::apply(&mut copy, fixes);
                Ok(copy)
            }
        };
        let checked = judged.and_then(|judged| {
            record.manifest.check(judged.clone())?;
            cuts::audit(judged)
        });
        let problem = match checked {
            Ok(0) => continue,
            Ok(_) => {
                leaked += 1;
                continue;
            }
            Err(problem) => problem,
        };
        let mut problem = format!("{}: {problem}", record.describe(&plan));
        if problems.len() < RETAINED {
            let kept = image.directory().join(format!(
                "cut-{}-{}.img",
                record.cut,
                cuts::STEPS[record.step].name
            ));
            current.store(&kept);
            problem.push_str(&format!(
                "\n  retained {}\n{}",
                kept.display(),
                verdict.report
            ));
        }
        problems.push(problem);
    }
    // The last step's manifest holds the files every earlier step made.
    let last = records.last().unwrap();
    assert!(
        last.manifest.contains("/Cut/a.bin"),
        "{name}: thin manifest"
    );
    cuts::assert_check_sees_a_changed_sentinel(&last.manifest, &current);
    assert!(
        problems.is_empty(),
        "{name}: {} of {} block cuts break the recovery contract (#188); every cut is \
         rebuildable from {}:\n{}",
        problems.len(),
        records.len(),
        image.directory().join("deltas.bin").display(),
        problems.join("\n")
    );
    println!(
        "[oracle cuts] {name}: {} cuts over {} steps; {clean} accepted as cut, {repaired} \
         refused and repaired; {leaked} leave leaked clusters; every untouched file intact",
        records.len(),
        cuts::STEPS.len()
    );
}

#[test]
fn fsck_exfat_accepts_formatted_volumes_at_both_sector_sizes_and_every_default_cluster_step() {
    linux_mkfs::fsck_accepts_formatted_volumes();
}

/// One guest call per image: fsck.exfat -n's status and dump.exfat's output,
/// with the separators the parser below reads.
struct Verdict {
    fsck_status: i32,
    fsck_output: String,
    dump: BTreeMap<String, String>,
}

fn ask_exfatprogs(image: &str) -> Verdict {
    let path = fixture(image);
    let script = format!(
        "{stage}set +e\n\
         fsck.exfat -n \"$image\" > \"$scratch/fsck.out\" 2>&1\n\
         echo \"fsck-status: $?\"\n\
         set -e\n\
         echo '--- fsck'\n\
         cat \"$scratch/fsck.out\"\n\
         echo '--- dump'\n\
         dump.exfat \"$image\" 2>&1 || true\n",
        stage = stage_image(&path)
    );
    let out = guest(
        "oracle",
        &format!("fsck.exfat -n and dump.exfat on {image}"),
        &script,
    )
    .success("the exfatprogs call");
    let (head, rest) = out.stdout.split_once("--- fsck\n").expect("fsck section");
    let (fsck_output, dump) = rest.split_once("--- dump\n").expect("dump section");
    let fsck_status = head
        .lines()
        .find_map(|line| line.strip_prefix("fsck-status: "))
        .expect("fsck status")
        .trim()
        .parse()
        .unwrap();
    println!("[oracle vm] fsck.exfat -n {image}: status {fsck_status}");
    let dump = dump
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(label, value)| (label.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Verdict {
        fsck_status,
        fsck_output: fsck_output.to_owned(),
        dump,
    }
}

#[test]
fn fsck_exfat_accepts_every_undamaged_fixture_and_refuses_structural_damage() {
    let mut checked = 0;
    for image in images() {
        let manifest = manifest(&image);
        let verdict = ask_exfatprogs(&image);
        match manifest.find("damage") {
            None => assert_eq!(
                verdict.fsck_status, 0,
                "{image}: fsck.exfat -n refuses an undamaged image:\n{}",
                verdict.fsck_output
            ),
            Some("boot" | "bitmap" | "upcase") => assert_ne!(
                verdict.fsck_status,
                0,
                "{image}: fsck.exfat -n accepts structural damage ({}):\n{}",
                manifest.get("damage"),
                verdict.fsck_output
            ),
            // Content is not a structure fsck can check: it passes, and the
            // file's hash is what shows the damage.
            Some("data") => {
                assert_eq!(
                    verdict.fsck_status, 0,
                    "{image}: fsck.exfat -n refuses a volume whose structures are intact:\n{}",
                    verdict.fsck_output
                );
                let damaged = kernel_listing(&image);
                let original = kernel_listing(manifest.get("origin"));
                let changed: Vec<_> = original
                    .iter()
                    .filter(|(path, row)| damaged.get(*path) != Some(row))
                    .map(|(path, _)| path.clone())
                    .collect();
                assert_eq!(
                    changed,
                    ["large.bin"],
                    "{image}: the overwritten data shows in exactly one file's hash"
                );
            }
            Some(other) => panic!("{image}: no expectation for damage {other}"),
        }
        checked += 1;
    }
    println!("[oracle vm] fsck.exfat -n judged {checked} fixtures");
}

#[test]
fn dump_exfat_agrees_with_this_crate_field_for_field() {
    let mut fields = 0;
    for image in images() {
        if manifest(&image).damaged() {
            continue;
        }
        let dump = ask_exfatprogs(&image).dump;
        let volume = open_path(&fixture(&image), 0).unwrap();
        let boot = volume.boot();
        let metadata = volume.metadata();
        let bitmap = metadata.bitmaps[0].as_ref().expect("an allocation bitmap");
        let free = u64::from(boot.cluster_count) - volume.bitmap().allocated_count();
        let ours = [
            ("Volume Length(sectors)", boot.volume_length.to_string()),
            ("FAT Offset(sector offset)", boot.fat_offset.to_string()),
            ("FAT Length(sectors)", boot.fat_length.to_string()),
            (
                "Cluster Heap Offset (sector offset)",
                boot.cluster_heap_offset.to_string(),
            ),
            ("Cluster Count", boot.cluster_count.to_string()),
            (
                "Root Cluster (cluster offset)",
                boot.root_cluster.to_string(),
            ),
            ("Volume Serial", format!("{:#x}", boot.volume_serial)),
            ("Sector Size Bits", boot.bytes_per_sector_shift.to_string()),
            (
                "Sector per Cluster bits",
                boot.sectors_per_cluster_shift.to_string(),
            ),
            ("Volume label", metadata.label.clone().unwrap_or_default()),
            ("Bitmap start cluster", bitmap.first_cluster.to_string()),
            ("Bitmap size", bitmap.length.to_string()),
            (
                "Upcase table start cluster",
                metadata.upcase.first_cluster.to_string(),
            ),
            ("Upcase table size", metadata.upcase.length.to_string()),
            ("Cluster size", boot.cluster_size().to_string()),
            ("Total Clusters", boot.cluster_count.to_string()),
            ("Free Clusters", free.to_string()),
        ];
        for (label, value) in ours {
            assert_eq!(
                dump.get(label),
                Some(&value),
                "{image}: dump.exfat's `{label}` disagrees with this crate"
            );
            fields += 1;
        }
    }
    println!("[oracle vm] dump.exfat agreed on {fields} fields");
}
