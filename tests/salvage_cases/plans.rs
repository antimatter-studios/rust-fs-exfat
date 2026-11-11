//! Materialization plans that name clusters, declare a final and a valid
//! length, and carry the rebuilt file's name and times.

use super::support::Bytes;
use super::{named, position, scan, volume, Failing};
use fs_core::BlockRead;
use fs_exfat::fixtures::{
    self, boot_field, BootField, CLUSTER, CLUSTER_COUNT, HEAP_OFFSET, SECTOR,
};
use fs_exfat::salvage::{
    ClusterLayout, ExfatSalvage, Extent, Feature, MaterializedRange, Plan, PlanFault, PlanMetadata,
    PlanStep, SalvageAdapter, SalvageError,
};

const C: u64 = CLUSTER as u64;

fn image_range(image: &[u8], cluster: u32, length: u64) -> &[u8] {
    let start = position(cluster) as usize;
    &image[start..start + length as usize]
}

fn clusters(first: u64, count: u64) -> PlanStep {
    PlanStep::Clusters { first, count }
}

#[test]
fn cluster_steps_rejoin_a_broken_chain_through_the_volume_geometry() {
    // short.bin's chain ends after cluster 70; its second half sits at 75.
    let image = fixtures::build_volume();
    let mut main_damaged = volume();
    let main = boot_field(
        main_damaged.image[..SECTOR].to_vec(),
        BootField::FileSystemName,
        b"NTFS    ",
    );
    main_damaged.image[..SECTOR].copy_from_slice(&main);
    let mut expected = image_range(&image, 70, C).to_vec();
    expected.extend_from_slice(image_range(&image, 75, C));

    for source in [Bytes(image.clone()), Bytes(main_damaged.image)] {
        let adapter = ExfatSalvage::probe(&source);
        let layout = adapter.cluster_layout().unwrap();
        assert_eq!(
            layout,
            ClusterLayout {
                first: 2,
                count: CLUSTER_COUNT.into(),
                size: C,
                offset: u64::from(HEAP_OFFSET) * SECTOR as u64,
            }
        );
        let mut plan = Plan::new(vec![clusters(70, 1), clusters(75, 1)]);
        plan.length = Some(2 * C);
        let mut output = Vec::new();
        let report = adapter.materialize(&source, &plan, &mut output).unwrap();
        assert_eq!(output, expected);
        assert_eq!(
            report.ranges,
            [
                MaterializedRange {
                    output: Extent::new(0, C),
                    source: Some(Extent::new(position(70), C)),
                    readable: true,
                },
                MaterializedRange {
                    output: Extent::new(C, C),
                    source: Some(Extent::new(position(75), C)),
                    readable: true,
                },
            ]
        );
        let cluster_of = |output| layout.cluster_at(report.source_of(output).unwrap());
        assert_eq!(cluster_of(C - 1), Some(70));
        assert_eq!(cluster_of(C + 5), Some(75));
    }

    // The last cluster of the heap is in range; the clusters around it are not.
    let source = Bytes(image);
    let adapter = ExfatSalvage::probe(&source);
    let last = u64::from(CLUSTER_COUNT) + 1;
    let plan = Plan::new(vec![clusters(last, 1)]);
    assert_eq!(
        adapter
            .materialize(&source, &plan, &mut Vec::new())
            .unwrap()
            .length,
        C
    );
    for (step, outside) in [
        clusters(1, 1),
        clusters(0, 3),
        clusters(last, 2),
        clusters(last + 1, 0),
        clusters(u64::MAX, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let mut steps = vec![PlanStep::Bytes(b"x".to_vec()); step];
        steps.push(outside);
        let mut output = Vec::new();
        let error = adapter
            .materialize(&source, &Plan::new(steps), &mut output)
            .unwrap_err();
        assert!(
            matches!(error, SalvageError::Plan(PlanFault::ClustersOutOfRange { step: index }) if index == step),
            "{error}"
        );
        assert!(output.is_empty());
    }
    assert_eq!(
        SalvageError::Plan(PlanFault::ClustersOutOfRange { step: 3 }).to_string(),
        "plan step 3 names clusters outside the volume"
    );

    let mut boot = adapter.boot().unwrap().clone();
    boot.bytes_per_sector_shift = 8;
    let blind = ExfatSalvage::from_boot(boot);
    assert!(matches!(
        blind.materialize(&source, &plan, &mut Vec::new()),
        Err(SalvageError::NoGeometry)
    ));
    assert!(matches!(
        plan.validate(source.size_bytes()),
        Err(SalvageError::Unsupported(Feature::ClusterSteps))
    ));
}

#[test]
fn a_final_length_cuts_the_last_cluster_and_bytes_past_the_valid_length_are_never_read() {
    // track.mp3 claims 3 clusters less 100 bytes, on the chain 30, 31, 33.
    let image = fixtures::build_volume();
    let length = 3 * C - 100;
    let valid = C + 10;
    let source = Failing {
        inner: Bytes(image.clone()),
        bad: vec![
            Extent::new(position(31) + 512, C - 512),
            Extent::new(position(33), C),
        ],
    };
    let adapter = ExfatSalvage::probe(&source);
    let mut plan = Plan::new(vec![clusters(30, 2), clusters(33, 1)]);
    plan.length = Some(length);
    plan.valid_length = Some(valid);
    assert_eq!(plan.length(), Some(length));
    let mut output = Vec::new();
    let report = adapter.materialize(&source, &plan, &mut output).unwrap();

    let mut expected = image_range(&image, 30, valid).to_vec();
    expected.resize(length as usize, 0);
    assert_eq!(output, expected);
    assert_eq!(report.length, length);
    assert_eq!(report.unreadable().count(), 0);
    assert_eq!(
        report.ranges,
        [
            MaterializedRange {
                output: Extent::new(0, valid),
                source: Some(Extent::new(position(30), valid)),
                readable: true,
            },
            MaterializedRange {
                output: Extent::new(valid, length - valid),
                source: None,
                readable: true,
            },
        ]
    );

    // Without a valid length, the cut last cluster is read up to the length.
    let source = Bytes(image.clone());
    plan.valid_length = None;
    let mut output = Vec::new();
    adapter.materialize(&source, &plan, &mut output).unwrap();
    let mut expected = image_range(&image, 30, 2 * C).to_vec();
    expected.extend_from_slice(image_range(&image, 33, C - 100));
    assert_eq!(output, expected);
    plan.length = Some(3 * C);
    plan.valid_length = Some(3 * C);
    assert_eq!(
        adapter
            .materialize(&source, &plan, &mut Vec::new())
            .unwrap()
            .length,
        3 * C
    );
}

#[test]
fn declared_lengths_that_disagree_with_the_steps_are_refused_before_anything_is_written() {
    let source = Bytes(fixtures::build_volume());
    let adapter = ExfatSalvage::probe(&source);
    let steps = || vec![clusters(30, 2), PlanStep::Zeros(0), clusters(33, 1)];
    for (length, valid_length, fault) in [
        (
            Some(3 * C + 1),
            None,
            PlanFault::LengthMismatch {
                length: 3 * C + 1,
                steps: 3 * C,
            },
        ),
        // Cutting cluster 33 off whole is a mismatch, not a trim.
        (
            Some(2 * C),
            None,
            PlanFault::LengthMismatch {
                length: 2 * C,
                steps: 3 * C,
            },
        ),
        (
            Some(0),
            None,
            PlanFault::LengthMismatch {
                length: 0,
                steps: 3 * C,
            },
        ),
        (
            None,
            Some(3 * C + 1),
            PlanFault::ValidLengthPastEnd {
                valid_length: 3 * C + 1,
                length: 3 * C,
            },
        ),
        (
            Some(2 * C + 1),
            Some(2 * C + 2),
            PlanFault::ValidLengthPastEnd {
                valid_length: 2 * C + 2,
                length: 2 * C + 1,
            },
        ),
    ] {
        let mut plan = Plan::new(steps());
        plan.length = length;
        plan.valid_length = valid_length;
        let mut output = Vec::new();
        let error = adapter
            .materialize(&source, &plan, &mut output)
            .unwrap_err();
        assert!(
            matches!(error, SalvageError::Plan(found) if found == fault),
            "{error}"
        );
        assert!(output.is_empty());
    }

    // Plans without clusters are checked the same way without an adapter.
    let mut plan = Plan::new(vec![PlanStep::Zeros(4), PlanStep::Bytes(b"tail".to_vec())]);
    plan.length = Some(4);
    let error = plan.validate(source.size_bytes()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "plan length 4 disagrees with the 8 byte(s) its steps produce"
    );
    plan.length = Some(5);
    plan.valid_length = Some(6);
    assert_eq!(
        plan.validate(source.size_bytes()).unwrap_err().to_string(),
        "plan valid length 6 is past its length 5"
    );
    plan.valid_length = Some(0);
    assert_eq!(plan.validate(source.size_bytes()).unwrap(), 5);
    assert_eq!(Plan::new(Vec::new()).validate(0).unwrap(), 0);
}

#[test]
fn plan_lengths_clusters_and_metadata_are_plain_serializable_data() {
    let source = Bytes(fixtures::build_volume());
    let (_, found) = scan(&source);
    let short = named(&found, "short.bin");
    let mut plan = Plan::new(vec![
        clusters(u64::from(short.detail.first_cluster), 1),
        clusters(75, 1),
    ]);
    plan.length = short.size;
    plan.valid_length = Some(short.detail.valid_data_length);
    plan.metadata = PlanMetadata {
        name: short.name.clone(),
        created: short.detail.created.clone(),
        modified: short.detail.modified.clone(),
        accessed: short.detail.accessed.clone(),
    };
    assert!(plan.metadata.created.is_some());

    let json = serde_json::to_string(&plan).unwrap();
    assert!(json.starts_with(
        r#"{"steps":[{"clusters":{"first":70,"count":1}},{"clusters":{"first":75,"count":1}}],"length":8192,"validLength":8192,"metadata":{"name":"short.bin","created":"#
    ));
    let back: Plan = serde_json::from_str(&json).unwrap();
    assert_eq!(back, plan);

    // A plan without the optional fields keeps its earlier form.
    let plain = Plan::new(vec![PlanStep::Zeros(2)]);
    assert_eq!(
        serde_json::to_string(&plain).unwrap(),
        r#"{"steps":[{"zeros":2}]}"#
    );
    assert!(plain.metadata.is_empty());
    let partial: Plan = serde_json::from_str(r#"{"steps":[],"metadata":{"name":"a"}}"#).unwrap();
    assert_eq!(partial.metadata.name.as_deref(), Some("a"));
    assert_eq!(partial.metadata.created, None);
}
