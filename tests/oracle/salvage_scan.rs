//! The salvage scan against the read-only macOS driver. Candidates, parent
//! claims, extent walks and materialization alone, with no strict mount,
//! must rebuild every file the driver returns from a macOS reference image.
//! The images' root directory grew after its files' data was written, so its
//! clusters are not contiguous and some entry sets cross from one root
//! cluster into a distant one (#99).

use super::{oracle_directory, reference};
use crate::support::Bytes;
use fs_core::BlockRead;
use fs_exfat::salvage::{
    Candidate, CandidateKind, CandidateState, EntrySetDetail, ExfatSalvage, Extent, Integrity,
    Parent, Plan, PlanStep, SalvageAdapter, Tree, WalkEnd,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use unicode_normalization::UnicodeNormalization;

/// The path of `candidate` through its single parent claims, or `None` for
/// an orphan, such as a stale set in a cluster no live directory claims.
fn path_of(
    candidate: &Candidate<EntrySetDetail>,
    candidates: &HashMap<u64, &Candidate<EntrySetDetail>>,
    tree: &Tree,
) -> Option<String> {
    let mut names = Vec::new();
    let mut current = candidate;
    loop {
        assert!(names.len() < 64, "directory cycle");
        names.push(current.name.clone().unwrap());
        let node = tree
            .nodes
            .iter()
            .find(|node| node.candidate == current.location.offset)
            .unwrap();
        match node.parents[..] {
            [] => return None,
            [Parent::Root] => break,
            [Parent::Candidate(offset)] => current = candidates[&offset],
            _ => panic!("{names:?} has conflicting parents {:?}", node.parents),
        }
    }
    names.reverse();
    Some(names.join("/").nfc().collect())
}

/// The file's bytes as the specification defines them: its DataLength, with
/// zeros from ValidDataLength on.
fn rebuild(
    adapter: &ExfatSalvage,
    source: &dyn BlockRead,
    file: &Candidate<EntrySetDetail>,
    path: &str,
) -> Vec<u8> {
    let walk = adapter.walk_extents(source, file).unwrap();
    assert_eq!(walk.end, WalkEnd::Complete, "{path}");
    let length = file.size.unwrap();
    let valid = file.detail.valid_data_length.min(length);
    let mut remaining = valid;
    let mut steps = Vec::new();
    for extent in walk.extents {
        let take = extent.length.min(remaining);
        if take > 0 {
            steps.push(PlanStep::Extent(Extent::new(extent.offset, take)));
        }
        remaining -= take;
    }
    assert_eq!(remaining, 0, "{path}");
    if length > valid {
        steps.push(PlanStep::Zeros(length - valid));
    }
    let plan = Plan::new(steps);
    let mut output = Vec::new();
    let report = adapter.materialize(source, &plan, &mut output).unwrap();
    assert_eq!(report.unreadable().count(), 0, "{path}");
    output
}

#[test]
fn salvage_candidates_rebuild_every_file_the_read_only_macos_driver_returns() {
    for sector in [512, 4096] {
        let (image, _) = reference(sector);
        let source = Bytes(image);
        let adapter = ExfatSalvage::probe(&source);
        let mut found = Vec::new();
        let report = adapter
            .find_candidates(&source, Extent::new(0, source.size_bytes()), &mut found)
            .unwrap();
        assert!(report.unreadable.is_empty());
        found.retain(|candidate| {
            candidate.state == CandidateState::Live && candidate.integrity == Integrity::Verified
        });
        let tree = adapter.rebuild_tree(&source, &found).unwrap();
        let by_offset: HashMap<u64, &Candidate<EntrySetDetail>> = found
            .iter()
            .map(|candidate| (candidate.location.offset, candidate))
            .collect();
        assert_eq!(by_offset.len(), found.len());

        let mut observed = BTreeMap::new();
        for file in found
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::File)
        {
            let Some(path) = path_of(file, &by_offset, &tree) else {
                continue;
            };
            let bytes = rebuild(&adapter, &source, file, &path);
            let value = format!("{}\t{:x}", bytes.len(), Sha256::digest(&bytes));
            assert!(observed.insert(path.clone(), value).is_none(), "{path}");
        }

        let expected: BTreeMap<String, String> =
            std::fs::read_to_string(oracle_directory().join(format!("files-{sector}.txt")))
                .unwrap()
                .lines()
                .map(|line| {
                    let fields: Vec<_> = line.splitn(6, '\t').collect();
                    (fields[5].nfc().collect(), fields[..2].join("\t"))
                })
                .collect();
        assert!(expected.len() > 185);
        let missing: Vec<_> = expected
            .keys()
            .filter(|path| !observed.contains_key(*path))
            .collect();
        assert!(
            missing.is_empty(),
            "{sector}-byte image: salvage rebuilt {} of the driver's {} files; missing {missing:?}",
            expected.len() - missing.len(),
            expected.len()
        );
        assert_eq!(observed, expected, "{sector}-byte image");
    }
}
