//! The salvage adapter over synthetic damaged images. Healthy forms of these
//! structures are checked against macOS by the primitive oracle tests.

use fs_core::BlockRead;
use fs_exfat::fixtures::{
    self, boot_field, entry_set, upcase_entry, upcase_table, BootField, Builder, CLUSTER,
    FAT_OFFSET, ROOT_CLUSTER, SECTOR,
};
use fs_exfat::primitives::{
    find_sets, BootSectorHeader, Limits, RawRootMetadata, ENTRY_SIZE, FAT_END,
};
use fs_exfat::salvage::{
    Candidate, CandidateKind, CandidateState, Capabilities, Damage, DamageKind, EntrySetDetail,
    ExfatSalvage, Extent, Feature, Integrity, MaterializedRange, Parent, Plan, PlanStep,
    SalvageAdapter, SalvageError, Severity, WalkEnd,
};
use std::io;
use std::ops::ControlFlow;

mod support;

#[path = "salvage_cases/stale_chains.rs"]
mod stale_chains;

#[path = "salvage_cases/uninterpreted.rs"]
mod uninterpreted;

#[path = "salvage_cases/upcase.rs"]
mod upcase;

#[path = "salvage_cases/copies.rs"]
mod copies;

#[path = "salvage_cases/secondary_count.rs"]
mod secondary_count;

#[path = "salvage_cases/cross_links.rs"]
mod cross_links;
#[path = "salvage_cases/fragmented.rs"]
mod fragmented;
#[path = "salvage_cases/plans.rs"]
mod plans;

use support::Bytes;

const FEATURES: [Feature; 9] = [
    Feature::FindCandidates,
    Feature::WalkExtents,
    Feature::RebuildTree,
    Feature::ReportDamage,
    Feature::ReadExtent,
    Feature::Materialize,
    Feature::DeletedCandidates,
    Feature::DeletedExtents,
    Feature::ClusterSteps,
];

/// A source whose reads fail wherever they touch a bad range.
struct Failing {
    inner: Bytes,
    bad: Vec<Extent>,
}

impl BlockRead for Failing {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let end = offset + output.len() as u64;
        if self
            .bad
            .iter()
            .any(|bad| offset < bad.offset + bad.length && bad.offset < end)
        {
            return Err(io::Error::other("unreadable sector").into());
        }
        self.inner.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }
}

fn volume() -> Builder {
    Builder {
        image: fixtures::build_volume(),
    }
}

fn position(cluster: u32) -> u64 {
    Builder::cluster_position(cluster) as u64
}

fn fat_word(cluster: u32) -> Extent {
    Extent::new(
        u64::from(FAT_OFFSET) * SECTOR as u64 + u64::from(cluster) * 4,
        4,
    )
}

fn whole(source: &dyn BlockRead) -> Extent {
    Extent::new(0, source.size_bytes())
}

fn scan(source: &dyn BlockRead) -> (ExfatSalvage, Vec<Candidate<EntrySetDetail>>) {
    let adapter = ExfatSalvage::probe(source);
    let mut found = Vec::new();
    let report = adapter
        .find_candidates(source, whole(source), &mut found)
        .unwrap();
    assert_eq!(report.candidates, found.len() as u64);
    assert_eq!(report.stopped_at, None);
    (adapter, found)
}

fn named<'a>(found: &'a [Candidate<EntrySetDetail>], name: &str) -> &'a Candidate<EntrySetDetail> {
    found
        .iter()
        .find(|candidate| candidate.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no candidate named {name}"))
}

fn damage(source: &dyn BlockRead) -> Vec<Damage> {
    let mut found = Vec::new();
    let report = ExfatSalvage::probe(source)
        .report_damage(source, &mut found)
        .unwrap();
    assert_eq!(report.reported, found.len() as u64);
    assert!(!report.stopped);
    found
}

fn has(found: &[Damage], kind: DamageKind, location: Extent, structure: &str) -> bool {
    found.iter().any(|damage| {
        damage.kind == kind && damage.location == location && damage.structure == structure
    })
}

#[test]
fn capabilities_name_what_is_missing() {
    let source = Bytes(fixtures::build_volume());
    let adapter = ExfatSalvage::probe(&source);
    assert_eq!(adapter.capabilities(), Capabilities::default());
    assert!(FEATURES
        .iter()
        .all(|feature| adapter.capabilities().supports(*feature)));

    let squashed = Capabilities {
        missing: vec![Feature::DeletedCandidates, Feature::DeletedExtents],
    };
    assert!(squashed.require(Feature::FindCandidates).is_ok());
    let error = squashed.require(Feature::DeletedExtents).unwrap_err();
    assert!(matches!(
        error,
        SalvageError::Unsupported(Feature::DeletedExtents)
    ));
    assert_eq!(
        error.to_string(),
        "salvage feature unsupported: deleted-extents"
    );
    let mut names: Vec<_> = FEATURES.iter().map(|feature| feature.to_string()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), FEATURES.len());
    assert!(names.iter().all(|name| !name.contains("fat")));
}

#[test]
fn a_scan_matches_the_primitive_finder() {
    let image = fixtures::reuse_image();
    let source = Bytes(image.clone());
    let (_, found) = scan(&source);
    let limits = Limits {
        cluster_count: fixtures::CLUSTER_COUNT,
        cluster_size: CLUSTER as u64,
    };
    let expected = find_sets(&image, image.len(), limits);
    assert_eq!(found.len(), expected.len());
    for (candidate, set) in found.iter().zip(&expected) {
        assert_eq!(candidate.location.offset, set.offset as u64);
        assert_eq!(candidate.location.length, (set.slots * ENTRY_SIZE) as u64);
        assert_eq!(candidate.name.as_deref(), Some(set.name.as_str()));
        assert_eq!(candidate.state == CandidateState::Live, set.in_use);
        assert_eq!(candidate.integrity == Integrity::Verified, set.checksum_ok);
        assert_eq!(candidate.kind == CandidateKind::Directory, set.is_dir);
        assert_eq!(candidate.size, Some(set.length));
        assert_eq!(candidate.detail.first_cluster, set.first_cluster);
        assert_eq!(candidate.detail.valid_data_length, set.valid_length);
        assert_eq!(candidate.detail.no_fat_chain, set.no_fat_chain);
        assert_eq!(candidate.detail.created, set.created);
    }
}

#[test]
fn scans_keep_deleted_and_damaged_records_as_evidence() {
    let source = Bytes(fixtures::build_volume());
    let (_, found) = scan(&source);

    let gone = named(&found, "gone");
    assert_eq!(gone.kind, CandidateKind::Directory);
    assert_eq!(gone.state, CandidateState::Deleted);
    assert_eq!(gone.integrity, Integrity::Verified);

    // The fixture alters one name character after sealing each of these sets.
    let broken = named(&found, "bXoken.jpg");
    assert_eq!(broken.state, CandidateState::Deleted);
    assert_eq!(broken.integrity, Integrity::Failed);
    assert_eq!(broken.location.offset, position(22));

    let lost = named(&found, "Xost-album");
    assert_eq!(
        (lost.kind, lost.state, lost.integrity),
        (
            CandidateKind::Directory,
            CandidateState::Live,
            Integrity::Failed
        )
    );

    let keep = named(&found, "keep.mp3");
    assert_eq!(keep.location, Extent::new(position(5), 96));
    assert_eq!(keep.size, Some(2 * CLUSTER as u64));
    assert_eq!(keep.detail.first_cluster, 10);
    assert!(keep.detail.no_fat_chain);
    assert_eq!(keep.detail.attributes, 0x20);
    assert_eq!(keep.detail.modified.as_deref(), Some("2022-09-05T17:53:54"));
}

#[test]
fn a_sink_can_stop_a_scan_and_resume_it() {
    let source = Bytes(fixtures::build_volume());
    let (adapter, all) = scan(&source);

    let mut first = Vec::new();
    let mut take_one = |candidate| {
        first.push(candidate);
        ControlFlow::Break(())
    };
    let report = adapter
        .find_candidates(&source, whole(&source), &mut take_one)
        .unwrap();
    assert_eq!(report.candidates, 1);
    let resume = report.stopped_at.unwrap();
    assert_eq!(resume, all[0].location.end().unwrap());

    let mut rest = Vec::new();
    let range = Extent::new(resume, source.size_bytes() - resume);
    adapter.find_candidates(&source, range, &mut rest).unwrap();
    first.extend(rest);
    assert_eq!(first, all);
}

#[test]
fn scans_start_on_entry_boundaries_and_stay_inside_the_source() {
    let source = Bytes(fixtures::build_volume());
    let (adapter, all) = scan(&source);
    let keep = named(&all, "keep.mp3").location.offset;

    let mut found = Vec::new();
    let range = Extent::new(keep - 31, 64);
    adapter.find_candidates(&source, range, &mut found).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].location.offset, keep);

    let size = source.size_bytes();
    for range in [Extent::new(size, 1), Extent::new(u64::MAX, 2)] {
        let error = adapter
            .find_candidates(&source, range, &mut Vec::new())
            .unwrap_err();
        assert!(matches!(error, SalvageError::Bounds));
    }
    let report = adapter
        .find_candidates(&source, Extent::new(size, 0), &mut found)
        .unwrap();
    assert_eq!(report.candidates, 0);
}

#[test]
fn sets_across_scan_windows_are_found() {
    let mut image = vec![0u8; 3 << 20];
    let set = entry_set("straddle.bin", false, 9, 100, true, true);
    let first = (1 << 20) - 64;
    let second = (2 << 20) + 32;
    image[first..first + set.len()].copy_from_slice(&set);
    image[second..second + set.len()].copy_from_slice(&set);
    let source = Bytes(image.clone());

    let adapter = ExfatSalvage::probe(&source);
    assert!(adapter.boot().is_none());
    let mut found = Vec::new();
    adapter
        .find_candidates(&source, whole(&source), &mut found)
        .unwrap();
    let offsets: Vec<_> = found
        .iter()
        .map(|candidate| candidate.location.offset)
        .collect();
    assert_eq!(offsets, [first as u64, second as u64]);
    let limits = Limits {
        cluster_count: 0xFFFF_FFF5,
        cluster_size: 1 << 25,
    };
    assert_eq!(find_sets(&image, image.len(), limits).len(), 2);
}

#[test]
fn scans_report_unreadable_sectors_and_skip_records_over_them() {
    let keep = position(5);
    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![Extent::new(keep, 1024)],
    };
    let adapter = ExfatSalvage::probe(&source);
    let mut found = Vec::new();
    let report = adapter
        .find_candidates(&source, whole(&source), &mut found)
        .unwrap();
    assert_eq!(report.unreadable, [Extent::new(keep, 1024)]);
    assert!(found
        .iter()
        .all(|candidate| candidate.name.as_deref() != Some("keep.mp3")));
    named(&found, "song.mp3");

    let mut found = Vec::new();
    let range = Extent::new(keep + 1024, 4096);
    let report = adapter.find_candidates(&source, range, &mut found).unwrap();
    assert!(report.unreadable.is_empty());
}

#[test]
fn scans_work_without_geometry_and_walks_say_so() {
    let mut image = fixtures::build_volume();
    image[..SECTOR * 24].fill(0);
    let source = Bytes(image);
    let (adapter, found) = scan(&source);
    assert!(adapter.boot().is_none());
    let keep = named(&found, "keep.mp3");
    assert!(matches!(
        adapter.walk_extents(&source, keep),
        Err(SalvageError::NoGeometry)
    ));
    assert!(matches!(
        adapter.rebuild_tree(&source, &found),
        Err(SalvageError::NoGeometry)
    ));
    assert_eq!(
        SalvageError::NoGeometry.to_string(),
        "no usable layout copy on the source"
    );

    let found = damage(&source);
    for structure in ["main boot region", "backup boot region"] {
        let offset = if structure.starts_with("main") {
            0
        } else {
            12 * SECTOR as u64
        };
        assert!(has(
            &found,
            DamageKind::Signature,
            Extent::new(offset + 3, 8),
            structure
        ));
        assert!(has(
            &found,
            DamageKind::Malformed,
            Extent::new(offset + 64, 56),
            structure
        ));
    }
}

#[test]
fn probing_reads_each_sector_size_and_either_copy() {
    for shift in [9, 12] {
        let adapter = ExfatSalvage::probe(&Bytes(support::volume(shift)));
        assert_eq!(adapter.boot().unwrap().bytes_per_sector_shift, shift);
    }

    let mut builder = volume();
    let main = boot_field(
        builder.image[..SECTOR].to_vec(),
        BootField::FileSystemName,
        b"NTFS    ",
    );
    builder.image[..SECTOR].copy_from_slice(&main);
    let source = Bytes(builder.image);
    let (adapter, found) = scan(&source);
    assert_eq!(adapter.boot().unwrap().percent_in_use, 59, "backup copy");
    let walk = adapter
        .walk_extents(&source, named(&found, "keep.mp3"))
        .unwrap();
    assert_eq!(walk.end, WalkEnd::Complete);

    let mut boot = adapter.boot().unwrap().clone();
    assert!(ExfatSalvage::from_boot(boot.clone()).boot().is_some());
    boot.bytes_per_sector_shift = 8;
    assert!(ExfatSalvage::from_boot(boot).boot().is_none());
}

#[test]
fn walks_follow_contiguous_and_fat_claims() {
    let source = Bytes(fixtures::build_volume());
    let (adapter, found) = scan(&source);

    let keep = adapter
        .walk_extents(&source, named(&found, "keep.mp3"))
        .unwrap();
    assert_eq!(
        keep.extents,
        [Extent::new(position(10), 2 * CLUSTER as u64)]
    );
    assert_eq!(keep.end, WalkEnd::Complete);

    let track = adapter
        .walk_extents(&source, named(&found, "track.mp3"))
        .unwrap();
    assert_eq!(
        track.extents,
        [
            Extent::new(position(30), 2 * CLUSTER as u64),
            Extent::new(position(33), CLUSTER as u64 - 100),
        ]
    );
    assert_eq!(track.end, WalkEnd::Complete);
    assert_eq!(track.mapped(), track.claimed);

    let deleted = adapter
        .walk_extents(&source, named(&found, ".track.mp3.abc123"))
        .unwrap();
    assert_eq!(deleted, track);

    let mut empty = named(&found, "notes.xyz").clone();
    empty.size = Some(0);
    let walk = adapter.walk_extents(&source, &empty).unwrap();
    assert!(walk.extents.is_empty());
    assert_eq!(walk.end, WalkEnd::Complete);
}

#[test]
fn broken_walks_keep_what_they_reached() {
    let mut builder = volume();
    builder.fat(31, 30);
    builder.fat(40, 41);
    builder.fat(41, 0x0FFF_0000);
    let source = Bytes(builder.image);
    let (adapter, found) = scan(&source);

    let short = adapter
        .walk_extents(&source, named(&found, "short.bin"))
        .unwrap();
    assert_eq!(short.extents, [Extent::new(position(70), CLUSTER as u64)]);
    assert_eq!(short.end, WalkEnd::Broken);

    let cycle = adapter
        .walk_extents(&source, named(&found, "track.mp3"))
        .unwrap();
    assert_eq!(
        cycle.extents,
        [Extent::new(position(30), 2 * CLUSTER as u64)]
    );
    assert_eq!(cycle.end, WalkEnd::Cycle);

    let mut reserved = named(&found, "photo.jpg").clone();
    reserved.detail.no_fat_chain = false;
    reserved.size = Some(3 * CLUSTER as u64);
    let walk = adapter.walk_extents(&source, &reserved).unwrap();
    assert_eq!(
        walk.extents,
        [Extent::new(position(40), 2 * CLUSTER as u64)]
    );
    assert_eq!(walk.end, WalkEnd::Broken);

    let mut edge = named(&found, "photo.jpg").clone();
    edge.detail.first_cluster = 200;
    edge.size = Some(4 * CLUSTER as u64);
    let walk = adapter.walk_extents(&source, &edge).unwrap();
    assert_eq!(
        walk.extents,
        [Extent::new(position(200), 2 * CLUSTER as u64)]
    );
    assert_eq!(walk.end, WalkEnd::OutOfRange);

    let mut boot = adapter.boot().unwrap().clone();
    boot.fat_length = 0;
    let walk = ExfatSalvage::from_boot(boot)
        .walk_extents(&source, named(&found, "short.bin"))
        .unwrap();
    assert_eq!(walk.extents, [Extent::new(position(70), CLUSTER as u64)]);
    assert_eq!(
        walk.end,
        WalkEnd::OutOfRange,
        "the FAT is too short to map it"
    );

    for (first, contiguous) in [(1, false), (1, true), (202, true)] {
        let mut outside = edge.clone();
        outside.detail.first_cluster = first;
        outside.detail.no_fat_chain = contiguous;
        let walk = adapter.walk_extents(&source, &outside).unwrap();
        assert!(walk.extents.is_empty());
        assert_eq!(walk.end, WalkEnd::OutOfRange);
    }
}

#[test]
fn walks_stop_at_unreadable_fat_sectors() {
    let fat = fat_word(0).offset;
    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![Extent::new(fat, 512)],
    };
    let (adapter, found) = scan(&source);
    let walk = adapter
        .walk_extents(&source, named(&found, "track.mp3"))
        .unwrap();
    assert_eq!(walk.extents, [Extent::new(position(30), CLUSTER as u64)]);
    assert_eq!(walk.end, WalkEnd::Unreadable(fat_word(30)));
}

#[test]
fn a_bad_fat_sector_spares_the_words_of_its_neighbours() {
    // The FAT's last sector holds no word a chain here uses; every walk reads
    // its words from the first sector, which reads (#107).
    let healthy = Bytes(fixtures::build_volume());
    let (adapter, found) = scan(&healthy);
    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![Extent::new(fat_word(0).offset + 7 * SECTOR as u64, 512)],
    };
    for candidate in &found {
        assert_eq!(
            adapter.walk_extents(&source, candidate).unwrap(),
            adapter.walk_extents(&healthy, candidate).unwrap(),
            "{:?}",
            candidate.name
        );
    }
    assert_eq!(
        adapter.rebuild_tree(&source, &found).unwrap(),
        adapter.rebuild_tree(&healthy, &found).unwrap()
    );
    assert!(!damage(&source)
        .iter()
        .any(|damage| damage.structure.starts_with("root directory")
            || damage.location == fat_word(ROOT_CLUSTER)));
}

#[test]
fn the_tree_keeps_parents_orphans_and_conflicts() {
    let mut builder = volume();
    builder.directory(
        27,
        &[entry_set("again", true, 21, CLUSTER as u64, true, true)],
    );
    let source = Bytes(builder.image);
    let (adapter, found) = scan(&source);
    let tree = adapter.rebuild_tree(&source, &found).unwrap();
    assert_eq!(tree.nodes.len(), found.len());
    let parents = |name: &str| {
        let offset = named(&found, name).location.offset;
        let node = tree
            .nodes
            .iter()
            .find(|node| node.candidate == offset)
            .unwrap();
        node.parents.clone()
    };
    let id = |name: &str| Parent::Candidate(named(&found, name).location.offset);

    assert_eq!(parents("visible"), [Parent::Root]);
    assert_eq!(parents("gone"), [Parent::Root]);
    assert_eq!(parents("keep.mp3"), [id("visible")]);
    assert_eq!(parents("deep.mp3"), [id("Xost-album")]);
    assert_eq!(parents("song.mp3"), [id("album"), id("again")]);
    assert!(parents("track.mp3").is_empty());
    assert!(parents("Xost-album").is_empty());

    let orphans: Vec<_> = tree.orphans().map(|node| node.candidate).collect();
    assert!(orphans.contains(&named(&found, "track.mp3").location.offset));
    let conflicts: Vec<_> = tree.conflicts().map(|node| node.candidate).collect();
    assert_eq!(conflicts, [named(&found, "song.mp3").location.offset]);
}

#[test]
fn damage_reports_name_only_what_is_wrong() {
    // The fixture volume with a real Up-case Table in place of its
    // placeholder of 100 zero bytes, which maps only 50 characters (#77):
    // Table 24, a-z to A-Z, then every other character to itself, so all of
    // 0000h to FFFFh (sections 7.2.5 and 7.2.5.1), in the compressed form.
    let mut words = vec![0xFFFF, 0x61];
    words.extend(0x41..=0x5A);
    words.extend([0xFFFF, 0xFFFF - 0x7A]);
    let table = upcase_table(&words);
    let mut builder = volume();
    builder.put(4, 0, &table);
    builder.put(ROOT_CLUSTER, 32, &upcase_entry(4, &table));
    let found = damage(&Bytes(builder.image));
    assert_eq!(found.len(), 2, "{found:?}");
    for (offset, structure) in [(0, "main boot region"), (12, "backup boot region")] {
        let sector = SECTOR as u64;
        let extended = Extent::new(offset * sector + sector, 8 * sector);
        assert!(has(&found, DamageKind::Signature, extended, structure));
    }
    assert!(found
        .iter()
        .all(|damage| damage.severity == Severity::Warning));
}

#[test]
fn damage_reports_cover_boot_copies_and_volume_flags() {
    let mut builder = volume();
    builder.image[SECTOR * 3] ^= 0xFF;
    builder.image[106] = 0x06;
    builder.image[510] = 0;
    builder.image[0] = 0;
    builder.image[20] = 1;
    let found = damage(&Bytes(builder.image));
    let main = "main boot region";
    let sector = SECTOR as u64;
    assert!(has(
        &found,
        DamageKind::Checksum,
        Extent::new(11 * sector, sector),
        main
    ));
    assert!(has(
        &found,
        DamageKind::Signature,
        Extent::new(510, 2),
        main
    ));
    assert!(has(&found, DamageKind::Signature, Extent::new(0, 3), main));
    assert!(has(
        &found,
        DamageKind::Malformed,
        Extent::new(11, 53),
        main
    ));
    assert!(has(
        &found,
        DamageKind::Mismatch,
        Extent::new(0, 24 * sector),
        "boot regions"
    ));
    assert!(!found
        .iter()
        .any(|damage| damage.structure == "volume flags"));

    let mut builder = volume();
    builder.image[106] = 0x06;
    let found = damage(&Bytes(builder.image));
    let flags: Vec<_> = found
        .iter()
        .filter(|damage| damage.structure == "volume flags")
        .map(|damage| (damage.kind, damage.severity, damage.location))
        .collect();
    assert_eq!(
        flags,
        [
            (DamageKind::Recorded, Severity::Warning, Extent::new(106, 2)),
            (DamageKind::Recorded, Severity::Notice, Extent::new(106, 2)),
        ]
    );
}

#[test]
fn damage_reports_cover_the_root_directory() {
    let root = position(ROOT_CLUSTER);
    let entry = |index: u64| Extent::new(root + index * 32, 32);

    let mut builder = volume();
    builder.image[root as usize] = 0x01;
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Missing,
        Extent::new(root, CLUSTER as u64),
        "root directory"
    ));

    let mut builder = volume();
    builder.image[root as usize + 1] = 0x04;
    builder.image[root as usize + 24] = 24;
    builder.image[root as usize + 36..root as usize + 40].copy_from_slice(&[1, 2, 3, 4]);
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Malformed,
        entry(0),
        "allocation bitmap entry"
    ));
    assert!(has(
        &found,
        DamageKind::Mismatch,
        entry(0),
        "allocation bitmap entry"
    ));
    assert!(has(
        &found,
        DamageKind::Checksum,
        Extent::new(position(4), 100),
        "upcase table"
    ));

    let mut builder = volume();
    builder.image[root as usize + 32 + 24..root as usize + 32 + 32]
        .copy_from_slice(&0u64.to_le_bytes());
    let found = damage(&Bytes(builder.image));
    assert!(has(&found, DamageKind::Malformed, entry(1), "upcase table"));

    let mut builder = volume();
    builder.image[root as usize + 32 + 24..root as usize + 32 + 32]
        .copy_from_slice(&(2 * CLUSTER as u64).to_le_bytes());
    let found = damage(&Bytes(builder.image.clone()));
    assert!(has(&found, DamageKind::Broken, fat_word(4), "upcase table"));
    builder.fat(4, FAT_END);
    let found = damage(&Bytes(builder.image));
    assert!(has(&found, DamageKind::Broken, fat_word(4), "upcase table"));

    let mut builder = volume();
    let upcase = position(4) as usize;
    // A run of 0 is read as no mappings (#75); this run passes FFFFh.
    builder.image[upcase..upcase + 8].copy_from_slice(&[0x41, 0, 0x42, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
    let checksum = fs_exfat::primitives::checksum32(&builder.image[upcase..upcase + 100]);
    builder.image[root as usize + 36..root as usize + 40].copy_from_slice(&checksum.to_le_bytes());
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Malformed,
        Extent::new(position(4), 100),
        "upcase table"
    ));
}

#[test]
fn damage_reports_cover_the_root_chain() {
    let mut builder = volume();
    builder.fat(ROOT_CLUSTER, 0);
    builder.image[position(ROOT_CLUSTER) as usize] = 0x01;
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Broken,
        fat_word(ROOT_CLUSTER),
        "root directory chain"
    ));
    assert!(
        !found
            .iter()
            .any(|damage| damage.kind == DamageKind::Missing),
        "a partial root is not evidence of a missing entry"
    );

    let mut builder = volume();
    builder.fat(ROOT_CLUSTER, ROOT_CLUSTER);
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Broken,
        fat_word(ROOT_CLUSTER),
        "root directory chain"
    ));

    let source = Bytes(fixtures::build_volume());
    let mut boot = ExfatSalvage::probe(&source).boot().unwrap().clone();
    boot.root_cluster = 500;
    let mut found = Vec::new();
    ExfatSalvage::from_boot(boot)
        .report_damage(&source, &mut found)
        .unwrap();
    assert!(has(
        &found,
        DamageKind::Malformed,
        Extent::new(96, 4),
        "root directory chain"
    ));

    // 32 MiB clusters make the 256 MiB directory limit eight clusters long.
    let mut builder = volume();
    for cluster in ROOT_CLUSTER..12 {
        builder.fat(cluster, cluster + 1);
    }
    builder.fat(12, FAT_END);
    let source = Bytes(builder.image);
    let mut boot = ExfatSalvage::probe(&source).boot().unwrap().clone();
    boot.sectors_per_cluster_shift = 16;
    let mut found = Vec::new();
    ExfatSalvage::from_boot(boot)
        .report_damage(&source, &mut found)
        .unwrap();
    let chain: Vec<_> = found
        .iter()
        .filter(|damage| damage.structure.starts_with("root directory"))
        .map(|damage| (damage.kind, damage.location))
        .collect();
    assert_eq!(chain[0], (DamageKind::Malformed, Extent::new(96, 4)));
    assert_eq!(chain[1].0, DamageKind::Unreadable);
}

#[test]
fn unreadable_structures_are_damage_not_errors() {
    let sector = SECTOR as u64;
    let root = position(ROOT_CLUSTER);
    let fat = fat_word(0).offset;
    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![Extent::new(12 * sector, sector), Extent::new(root, 512)],
    };
    let found = damage(&source);
    assert!(has(
        &found,
        DamageKind::Unreadable,
        Extent::new(12 * sector, 12 * sector),
        "backup boot region"
    ));
    assert!(has(
        &found,
        DamageKind::Unreadable,
        Extent::new(root, CLUSTER as u64),
        "root directory"
    ));
    assert!(!found
        .iter()
        .any(|damage| damage.structure == "boot regions"));

    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![Extent::new(fat, 512), Extent::new(position(4), 512)],
    };
    let found = damage(&source);
    assert!(has(
        &found,
        DamageKind::Unreadable,
        fat_word(ROOT_CLUSTER),
        "root directory chain"
    ));

    let mut builder = volume();
    builder.image[position(ROOT_CLUSTER) as usize + 32 + 24] = 0;
    builder.image[position(ROOT_CLUSTER) as usize + 32 + 25] = 0x20;
    builder.fat(4, 5);
    builder.fat(5, FAT_END);
    let source = Failing {
        inner: Bytes(builder.image),
        bad: vec![Extent::new(position(5), 512)],
    };
    let found = damage(&source);
    assert!(has(
        &found,
        DamageKind::Unreadable,
        Extent::new(position(4), 2 * CLUSTER as u64),
        "upcase table"
    ));
}

#[test]
fn a_sink_can_stop_a_damage_report() {
    let source = Bytes(fixtures::build_volume());
    let mut seen = 0;
    let mut stop = |_damage: Damage| {
        seen += 1;
        ControlFlow::Break(())
    };
    let report = ExfatSalvage::probe(&source)
        .report_damage(&source, &mut stop)
        .unwrap();
    assert!(report.stopped);
    assert_eq!(report.reported, 1);
    assert_eq!(seen, 1);
}

#[test]
fn extents_read_at_offsets_inside_their_bounds() {
    let image = fixtures::build_volume();
    let source = Bytes(image.clone());
    let adapter = ExfatSalvage::probe(&source);
    let extent = Extent::new(position(10), 100);

    let mut output = [0u8; 8];
    assert_eq!(
        adapter
            .read_extent(&source, extent, 16, &mut output)
            .unwrap(),
        8
    );
    assert_eq!(output, image[position(10) as usize + 16..][..8]);

    let mut output = [0u8; 64];
    assert_eq!(
        adapter
            .read_extent(&source, extent, 90, &mut output)
            .unwrap(),
        10
    );
    assert_eq!(
        adapter
            .read_extent(&source, extent, 100, &mut output)
            .unwrap(),
        0
    );
    let size = source.size_bytes();
    for (extent, offset) in [
        (extent, 101),
        (Extent::new(size - 10, 20), 0),
        (Extent::new(u64::MAX, 2), 0),
    ] {
        assert!(matches!(
            adapter.read_extent(&source, extent, offset, &mut output),
            Err(SalvageError::Bounds)
        ));
    }

    let failing = Failing {
        inner: Bytes(image),
        bad: vec![Extent::new(position(10), 512)],
    };
    let error = adapter
        .read_extent(&failing, extent, 0, &mut output)
        .unwrap_err();
    assert!(matches!(error, SalvageError::Device(_)));
    assert!(error.to_string().starts_with("source read failed"));
    assert!(std::error::Error::source(&error).is_some());
    assert!(std::error::Error::source(&SalvageError::Bounds).is_none());
    assert_eq!(
        SalvageError::Bounds.to_string(),
        "range outside the source or not representable"
    );
}

#[test]
fn plans_materialize_with_provenance() {
    let image = fixtures::build_volume();
    let source = Bytes(image.clone());
    let adapter = ExfatSalvage::probe(&source);
    let start = position(30);
    let plan = Plan::new(vec![
        PlanStep::Bytes(b"head".to_vec()),
        PlanStep::Extent(Extent::new(start, 70_000)),
        PlanStep::Extent(Extent::new(start + 70_000, 30)),
        PlanStep::Zeros(0),
        PlanStep::Zeros(70_000),
        PlanStep::Bytes(Vec::new()),
        PlanStep::Bytes(b"tail".to_vec()),
    ]);
    let mut output = Vec::new();
    let report = adapter.materialize(&source, &plan, &mut output).unwrap();

    let mut expected = b"head".to_vec();
    expected.extend_from_slice(&image[start as usize..start as usize + 70_030]);
    expected.extend(std::iter::repeat_n(0, 70_000));
    expected.extend_from_slice(b"tail");
    assert_eq!(output, expected);
    assert_eq!(report.length, expected.len() as u64);
    assert_eq!(plan.length(), Some(report.length));
    assert_eq!(
        report.ranges,
        [
            MaterializedRange {
                output: Extent::new(0, 4),
                source: None,
                readable: true
            },
            MaterializedRange {
                output: Extent::new(4, 70_030),
                source: Some(Extent::new(start, 70_030)),
                readable: true
            },
            MaterializedRange {
                output: Extent::new(70_034, 70_004),
                source: None,
                readable: true
            },
        ]
    );
    assert_eq!(report.source_of(4), Some(start));
    assert_eq!(report.source_of(70_033), Some(start + 70_029));
    assert_eq!(report.source_of(0), None);
    assert_eq!(report.source_of(report.length), None);
}

#[test]
fn unreadable_plan_bytes_are_flagged_and_the_rest_still_runs() {
    let image = fixtures::build_volume();
    let start = position(30);
    let source = Failing {
        inner: Bytes(image.clone()),
        bad: vec![Extent::new(start + 1024, 512)],
    };
    let adapter = ExfatSalvage::probe(&source);
    let plan = Plan::new(vec![PlanStep::Extent(Extent::new(start, 2048))]);
    let mut output = Vec::new();
    let report = adapter.materialize(&source, &plan, &mut output).unwrap();
    assert_eq!(
        report.unreadable().collect::<Vec<_>>(),
        [Extent::new(1024, 512)]
    );
    assert_eq!(output[..1024], image[start as usize..start as usize + 1024]);
    assert!(output[1024..1536].iter().all(|byte| *byte == 0));
    assert_eq!(
        output[1536..],
        image[start as usize + 1536..start as usize + 2048]
    );
    assert_eq!(report.ranges.len(), 3);
    assert_eq!(report.source_of(1100), Some(start + 1100));
}

#[test]
fn plans_are_checked_before_anything_is_written() {
    let source = Bytes(fixtures::build_volume());
    let adapter = ExfatSalvage::probe(&source);
    let size = source.size_bytes();
    for steps in [
        vec![
            PlanStep::Bytes(b"x".to_vec()),
            PlanStep::Extent(Extent::new(size - 4, 8)),
        ],
        vec![PlanStep::Extent(Extent::new(u64::MAX, 2))],
        vec![PlanStep::Zeros(u64::MAX), PlanStep::Zeros(1)],
    ] {
        let mut output = Vec::new();
        let error = adapter
            .materialize(&source, &Plan::new(steps), &mut output)
            .unwrap_err();
        assert!(matches!(error, SalvageError::Bounds));
        assert!(output.is_empty());
    }

    struct Full;
    impl io::Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    for step in [
        PlanStep::Bytes(b"x".to_vec()),
        PlanStep::Zeros(1),
        PlanStep::Extent(Extent::new(0, 1)),
    ] {
        let plan = Plan::new(vec![step]);
        let error = adapter.materialize(&source, &plan, &mut Full).unwrap_err();
        assert!(matches!(error, SalvageError::Sink(_)));
        assert_eq!(error.to_string(), "output sink failed: disk full");
        assert!(std::error::Error::source(&error).is_some());
    }
}

#[test]
fn evidence_and_plans_are_plain_serializable_data() {
    let source = Bytes(fixtures::build_volume());
    let (adapter, found) = scan(&source);
    let json = serde_json::to_string(&found).unwrap();
    let back: Vec<Candidate<EntrySetDetail>> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, found);
    assert!(json.contains(r#""state":"deleted""#));
    assert!(json.contains(r#""integrity":"failed""#));

    let walk = adapter.walk_extents(&source, &back[0]).unwrap();
    let tree = adapter.rebuild_tree(&source, &back).unwrap();
    let damage = damage(&source);
    for value in [
        serde_json::to_value(&walk).unwrap(),
        serde_json::to_value(&tree).unwrap(),
        serde_json::to_value(&damage).unwrap(),
        serde_json::to_value(adapter.capabilities()).unwrap(),
    ] {
        assert!(!value.is_null());
    }

    let plan: Plan = serde_json::from_str(
        r#"{"steps":[{"extent":{"offset":28672,"length":4}},{"zeros":2},{"bytes":[1,2]}]}"#,
    )
    .unwrap();
    assert_eq!(
        plan.steps,
        [
            PlanStep::Extent(Extent::new(28672, 4)),
            PlanStep::Zeros(2),
            PlanStep::Bytes(vec![1, 2]),
        ]
    );
    assert_eq!(plan.validate(source.size_bytes()).unwrap(), 8);
}

#[test]
fn root_entries_keep_their_offsets() {
    let image = fixtures::build_volume();
    let root = position(ROOT_CLUSTER) as usize;
    let metadata = RawRootMetadata::parse(&image[root..root + CLUSTER]);
    let offsets: Vec<_> = metadata.entries.iter().map(|entry| entry.offset).collect();
    assert_eq!(offsets, [0, 32, 64, 160, 192, 224]);
    let header = BootSectorHeader::parse(&image).unwrap();
    assert_eq!(header.sector.root_cluster, ROOT_CLUSTER);
}

#[test]
fn second_fat_volumes_use_the_active_fat_and_need_both_bitmaps() {
    let source = Bytes(fixtures::build_volume());
    let mut boot = ExfatSalvage::probe(&source).boot().unwrap().clone();
    boot.number_of_fats = 2;
    let mut found = Vec::new();
    ExfatSalvage::from_boot(boot.clone())
        .report_damage(&source, &mut found)
        .unwrap();
    let missing: Vec<_> = found
        .iter()
        .filter(|damage| damage.kind == DamageKind::Missing)
        .map(|damage| damage.detail.as_str())
        .collect();
    assert_eq!(missing, ["no allocation bitmap entry for FAT 1"]);

    // The second FAT starts where the first one ends.
    boot.volume_flags = 1;
    let second = fat_word(ROOT_CLUSTER).offset + u64::from(boot.fat_length) * SECTOR as u64;
    let mut found = Vec::new();
    ExfatSalvage::from_boot(boot)
        .report_damage(&source, &mut found)
        .unwrap();
    assert!(has(
        &found,
        DamageKind::Broken,
        Extent::new(second, 4),
        "root directory chain"
    ));
}

#[test]
fn root_entries_are_located_across_root_clusters() {
    let mut builder = volume();
    let root = position(ROOT_CLUSTER) as usize;
    // Deleted entries keep the root going until it continues in cluster 8.
    for slot in (root + 256..root + CLUSTER).step_by(32) {
        builder.image[slot] = 0x05;
    }
    builder.fat(ROOT_CLUSTER, 8);
    builder.fat(8, FAT_END);
    let upcase = fixtures::special(0x02, 4, 100);
    builder.put(8, 0, &upcase);
    builder.image[root + 32] = 0x02;
    let mut bitmap = fixtures::bitmap_entry(fixtures::BITMAP_CLUSTER, 25);
    bitmap[1] = 0x02;
    builder.put(8, 32, &bitmap);
    let found = damage(&Bytes(builder.image));
    assert!(has(
        &found,
        DamageKind::Missing,
        Extent::new(root as u64, CLUSTER as u64),
        "root directory"
    ));
    assert!(has(
        &found,
        DamageKind::Malformed,
        Extent::new(position(8) + 32, 32),
        "allocation bitmap entry"
    ));
}

/// Stops the damage report after each finding in turn and checks that the
/// findings so far are exactly the start of the full report.
fn stops_after_every_finding(adapter: &ExfatSalvage, source: &dyn BlockRead) -> usize {
    let mut all = Vec::new();
    adapter.report_damage(source, &mut all).unwrap();
    for count in 1..=all.len() {
        let mut seen = Vec::new();
        let mut sink = |damage| {
            seen.push(damage);
            if seen.len() == count {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let report = adapter.report_damage(source, &mut sink).unwrap();
        assert!(report.stopped);
        assert_eq!(report.reported, count as u64);
        assert_eq!(seen, all[..count]);
    }
    all.len()
}

#[test]
fn damage_reports_stop_cleanly_after_any_finding() {
    let root = position(ROOT_CLUSTER) as usize;

    let mut builder = volume();
    builder.image[SECTOR * 3] ^= 0xFF;
    builder.image[0] = 0;
    builder.image[20] = 1;
    builder.image[510] = 0;
    let source = Bytes(builder.image);
    assert!(stops_after_every_finding(&ExfatSalvage::probe(&source), &source) >= 7);

    let mut builder = volume();
    builder.image[106] = 0x06;
    builder.image[root + 1] = 0x04;
    builder.image[root + 24] = 24;
    let upcase = position(4) as usize;
    // A run of 0 is read as no mappings (#75); this run passes FFFFh.
    builder.image[upcase..upcase + 8].copy_from_slice(&[0x41, 0, 0x42, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
    let source = Bytes(builder.image);
    assert!(stops_after_every_finding(&ExfatSalvage::probe(&source), &source) >= 8);

    let mut builder = volume();
    builder.image[root] = 0x01;
    builder.image[root + 32] = 0x02;
    let source = Bytes(builder.image);
    assert!(stops_after_every_finding(&ExfatSalvage::probe(&source), &source) >= 4);

    let source = Failing {
        inner: Bytes(fixtures::build_volume()),
        bad: vec![
            Extent::new(0, 512),
            Extent::new(12 * SECTOR as u64, 512),
            Extent::new(root as u64, 512),
        ],
    };
    let adapter = ExfatSalvage::from_boot(
        ExfatSalvage::probe(&Bytes(fixtures::build_volume()))
            .boot()
            .unwrap()
            .clone(),
    );
    assert_eq!(stops_after_every_finding(&adapter, &source), 3);

    let mut builder = volume();
    for cluster in ROOT_CLUSTER..12 {
        builder.fat(cluster, cluster + 1);
    }
    builder.fat(12, FAT_END);
    let source = Bytes(builder.image);
    let mut boot = ExfatSalvage::probe(&source).boot().unwrap().clone();
    boot.sectors_per_cluster_shift = 16;
    assert!(stops_after_every_finding(&ExfatSalvage::from_boot(boot), &source) >= 2);
}
