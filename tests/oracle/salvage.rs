//! The salvage adapter's verbs, run on the images macOS formatted and wrote,
//! and compared with what the read-only macOS driver listed and read there
//! (#97). The primitives under them are checked in `primitives.rs`; these
//! check the verbs a salvage caller sees.

use crate::support::Bytes;
use fs_core::BlockRead;
use fs_exfat::salvage::{
    Candidate, CandidateKind, CandidateState, Damage, EntrySetDetail, ExfatSalvage, Extent,
    Integrity, Parent, Plan, PlanStep, SalvageAdapter, Tree, WalkEnd,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use unicode_normalization::UnicodeNormalization;

/// Bytes per `read_extent` call: no multiple of a sector, so the reads start
/// and end inside sectors and clusters.
const READ_STEP: usize = 1000;

fn oracle_directory() -> PathBuf {
    std::env::var("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle on macOS to create independent reference images")
        .into()
}

/// The driver's own records, one per line, keyed by NFC path: the listing's
/// kind and length, and each file's length and SHA-256 as the driver read it.
fn driver_records(
    name: &str,
    sector: u64,
    key: usize,
    value: &[usize],
) -> BTreeMap<String, String> {
    std::fs::read_to_string(oracle_directory().join(format!("{name}-{sector}.txt")))
        .unwrap()
        .lines()
        .map(|line| {
            let fields: Vec<_> = line.splitn(key + 1, '\t').collect();
            let kept: Vec<_> = value.iter().map(|&index| fields[index]).collect();
            (fields[key].nfc().collect(), kept.join("\t"))
        })
        .collect()
}

/// A live candidate's path, through the one parent the rebuilt tree gives it
/// and each parent's own path.
fn path_of(
    offset: u64,
    candidates: &HashMap<u64, &Candidate<EntrySetDetail>>,
    tree: &HashMap<u64, &[Parent]>,
    depth: usize,
) -> String {
    assert!(depth < 64, "a cycle of parent claims at byte {offset}");
    let candidate = candidates[&offset];
    let name: String = candidate.name.as_deref().unwrap().nfc().collect();
    match tree[&offset] {
        [Parent::Root] => name,
        [Parent::Candidate(parent)] => {
            assert_eq!(candidates[parent].kind, CandidateKind::Directory);
            assert_eq!(candidates[parent].state, CandidateState::Live);
            format!("{}/{name}", path_of(*parent, candidates, tree, depth + 1))
        }
        parents => panic!("{name} at byte {offset} has parents {parents:?}"),
    }
}

/// A file's bytes as a salvage caller rebuilds them: its walked extents up to
/// ValidDataLength, then zeros to DataLength, which section 7.6.5 of the
/// specification says reads past ValidDataLength return.
fn plan(walk_extents: &[Extent], length: u64, valid: u64) -> Plan {
    // Built on a default plan, so the fields #100 adds keep their defaults.
    let mut plan = Plan::default();
    let mut remaining = valid.min(length);
    for extent in walk_extents {
        let take = extent.length.min(remaining);
        if take > 0 {
            plan.steps
                .push(PlanStep::Extent(Extent::new(extent.offset, take)));
        }
        remaining -= take;
    }
    assert_eq!(remaining, 0, "the walk maps less than ValidDataLength");
    if length > valid {
        plan.steps.push(PlanStep::Zeros(length - valid));
    }
    plan
}

/// The same bytes through `read_extent` alone, in reads that start inside an
/// extent and cross sector and cluster boundaries.
fn read_through_extents(
    adapter: &ExfatSalvage,
    source: &dyn BlockRead,
    walk_extents: &[Extent],
    length: u64,
    valid: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; READ_STEP];
    for extent in walk_extents {
        let mut offset = 0;
        while offset < extent.length {
            let count = adapter
                .read_extent(source, *extent, offset, &mut buffer)
                .unwrap();
            assert_eq!(count as u64, (extent.length - offset).min(READ_STEP as u64));
            bytes.extend_from_slice(&buffer[..count]);
            offset += count as u64;
        }
    }
    assert_eq!(bytes.len() as u64, length);
    bytes[valid.min(length) as usize..].fill(0);
    bytes
}

fn digest(bytes: &[u8]) -> String {
    format!("{}\t{:x}", bytes.len(), Sha256::digest(bytes))
}

/// Every live file and directory the read-only macOS driver lists is a live,
/// checksum-verified candidate in the tree salvage rebuilds, under the same
/// parent, with no orphan or conflicting claim; each file's walked extents, materialized or read piecewise and cut
/// at ValidDataLength, hash as the driver read it; and the damage report
/// finds nothing on a volume `fsck_exfat -n` passed. The images' root
/// directories grew after their files' data was written, so some entry sets
/// cross from one root cluster into a distant one (#99), and the scan finds
/// those too.
#[test]
fn salvage_verbs_rebuild_what_the_read_only_macos_driver_lists_and_reads() {
    for sector in [512, 4096] {
        let path = oracle_directory().join(format!("reference-{sector}.dmg"));
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file());
        let source = Bytes(std::fs::read(path).unwrap());
        let adapter = ExfatSalvage::probe(&source);
        assert_eq!(adapter.boot().unwrap().bytes_per_sector(), sector);
        assert!(adapter.capabilities().missing.is_empty());

        let mut damage: Vec<Damage> = Vec::new();
        let report = adapter.report_damage(&source, &mut damage).unwrap();
        assert_eq!(damage, [], "{sector}: damage on a volume fsck_exfat passed");
        assert_eq!((report.reported, report.stopped), (0, false));

        let mut candidates: Vec<Candidate<EntrySetDetail>> = Vec::new();
        let whole = Extent::new(0, source.size_bytes());
        let scan = adapter
            .find_candidates(&source, whole, &mut candidates)
            .unwrap();
        assert_eq!(scan.candidates, candidates.len() as u64);
        assert_eq!((scan.unreadable, scan.stopped_at), (vec![], None));
        // macOS leaves no deleted set behind on these volumes, and nothing in
        // the files it wrote passes for an entry set: every candidate the scan
        // reports is a live, checksum-verified set.
        let unexpected: Vec<_> = candidates
            .iter()
            .filter(|candidate| {
                candidate.state != CandidateState::Live
                    || candidate.integrity != Integrity::Verified
            })
            .collect();
        assert!(unexpected.is_empty(), "{sector}: {unexpected:?}");

        let tree: Tree = adapter.rebuild_tree(&source, &candidates).unwrap();
        assert_eq!(tree.nodes.len(), candidates.len());
        let by_offset: HashMap<_, _> = candidates
            .iter()
            .map(|candidate| (candidate.location.offset, candidate))
            .collect();
        let parents: HashMap<_, _> = tree
            .nodes
            .iter()
            .map(|node| (node.candidate, node.parents.as_slice()))
            .collect();
        let orphans: Vec<_> = tree.orphans().collect();
        let conflicts: Vec<_> = tree.conflicts().collect();
        assert!(orphans.is_empty(), "{sector}: orphans {orphans:?}");
        assert!(conflicts.is_empty(), "{sector}: conflicts {conflicts:?}");

        let mut listed = BTreeMap::new();
        let mut contents = BTreeMap::new();
        for candidate in &candidates {
            let path = path_of(candidate.location.offset, &by_offset, &parents, 0);
            let length = candidate.size.unwrap();
            let directory = candidate.kind == CandidateKind::Directory;
            let record = if directory {
                "d\t0".to_owned()
            } else {
                format!("f\t{length}")
            };
            let walk = adapter.walk_extents(&source, candidate).unwrap();
            assert_eq!(walk.claimed, length, "{sector}: {path}");
            assert_eq!(walk.mapped(), length, "{sector}: {path}");
            if length > 0 {
                assert_eq!(walk.end, WalkEnd::Complete, "{sector}: {path}");
            }
            if !directory {
                let valid = candidate.detail.valid_data_length;
                let mut output = Vec::new();
                let rebuilt = adapter
                    .materialize(&source, &plan(&walk.extents, length, valid), &mut output)
                    .unwrap();
                assert_eq!(rebuilt.length, length, "{sector}: {path}");
                assert_eq!(rebuilt.unreadable().count(), 0, "{sector}: {path}");
                let pieces = read_through_extents(&adapter, &source, &walk.extents, length, valid);
                assert_eq!(digest(&pieces), digest(&output), "{sector}: {path}");
                assert!(contents.insert(path.clone(), digest(&output)).is_none());
            }
            assert!(
                listed.insert(path, record).is_none(),
                "{sector}: listed twice"
            );
        }

        let driver_listing = driver_records("listing", sector, 2, &[0, 1]);
        let driver_files = driver_records("files", sector, 5, &[0, 1]);
        let missed: BTreeSet<_> = driver_listing
            .keys()
            .filter(|path| !listed.contains_key(*path))
            .collect();
        assert!(
            missed.is_empty(),
            "{sector}: driver entries with no candidate {missed:?}"
        );
        assert!(listed.len() > 185);
        assert_eq!(listed, driver_listing, "{sector}: listing");
        assert_eq!(contents, driver_files, "{sector}: contents");
    }
}
