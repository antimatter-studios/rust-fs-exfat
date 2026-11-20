#![allow(deprecated)]

use fs_core::layers::salvage as shared;
use fs_exfat::salvage::adapter::{Candidate, SalvageAdapter, SalvageError};
use fs_exfat::salvage::{
    EntrySetDetail, ExfatSalvage, Extent, ExtentWalk, Parent, Plan, PlanStep, TreeNode, WalkEnd,
};

mod support;

#[test]
fn legacy_exfat_adapter_preserves_walks_parents_and_errors() {
    let mut image = support::metadata_volume();
    let location = 32 * 512 + 96;
    let set = support::file_set("sample.bin", 0x20, 5, 16, true);
    image[location..location + set.len()].copy_from_slice(&set);
    let source = support::Bytes(image);
    let adapter = ExfatSalvage::probe(&source);
    let mut found: Vec<Candidate<EntrySetDetail>> = Vec::new();
    adapter
        .find_candidates(&source, Extent::new(32 * 512, 512), &mut found)
        .unwrap();
    assert_eq!(found.len(), 1);
    let walk: ExtentWalk = adapter.walk_extents(&source, &found[0]).unwrap();
    assert_eq!(walk.extents, vec![Extent::new(35 * 512, 16)]);
    assert_eq!(walk.end, WalkEnd::Complete);
    let tree = adapter.rebuild_tree(&source, &found).unwrap();
    assert_eq!(
        tree.nodes,
        [TreeNode {
            candidate: location as u64,
            parents: vec![Parent::Root]
        }]
    );
    let plan = Plan::new(vec![PlanStep::Extent(walk.extents[0])]);
    let reader = adapter.plan_reader(&source, &plan).unwrap();
    assert_eq!(reader.len(), 16);
    let error: fs_exfat::salvage::SalvageError = adapter
        .read_extent(&source, Extent::new(u64::MAX, 1), 0, &mut [0])
        .unwrap_err();
    assert!(matches!(error, SalvageError::Bounds));
    assert!(matches!(
        SalvageError::try_from(shared::SalvageError::from(error)),
        Ok(SalvageError::Bounds)
    ));

    let candidates = found.into_iter().map(Into::into).collect::<Vec<_>>();
    let shared_walk =
        shared::SalvageAdapter::walk_extents(&adapter, &source, &candidates[0]).unwrap();
    assert_eq!(shared_walk, shared::ExtentWalk::from(walk.clone()));
    assert_eq!(ExtentWalk::try_from(shared_walk).unwrap(), walk);
    let shared_tree = shared::SalvageAdapter::rebuild_tree(&adapter, &source, &candidates).unwrap();
    assert_eq!(
        shared_tree.nodes[0].parents[0].name.as_deref(),
        Some(b"sample.bin".as_slice())
    );
    assert_eq!(
        shared_tree.nodes[0].parents[0].state,
        shared::CandidateState::Live
    );
    assert_eq!(fs_exfat::salvage::Tree::from(shared_tree), tree);
    let shared_plan = shared::Plan::from(plan.clone());
    assert_eq!(shared_plan.digest(), plan.digest());
    assert_eq!(Plan::try_from(shared_plan).unwrap(), plan);
}

#[test]
fn sparse_walks_and_new_features_have_no_legacy_equivalent() {
    let sparse = shared::ExtentWalk {
        extents: vec![shared::WalkExtent::stored(8, shared::Extent::new(512, 8))],
        claimed: 16,
        end: shared::WalkEnd::Complete,
    };
    assert_eq!(ExtentWalk::try_from(sparse.clone()), Err(sparse));
    assert_eq!(
        fs_exfat::salvage::Feature::try_from(shared::Feature::EncodedExtents),
        Err(shared::Feature::EncodedExtents),
    );
}
