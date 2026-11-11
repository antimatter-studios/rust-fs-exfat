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

/// The report's ranges cut to `[offset, offset + length)`.
fn clipped(ranges: &[MaterializedRange], offset: u64, length: u64) -> Vec<MaterializedRange> {
    let end = offset + length;
    ranges
        .iter()
        .filter(|range| range.output.offset < end && offset < range.output.end().unwrap())
        .map(|range| {
            let start = range.output.offset.max(offset);
            let stop = range.output.end().unwrap().min(end);
            MaterializedRange {
                output: Extent::new(start, stop - start),
                source: range.source.map(|source| {
                    Extent::new(source.offset + start - range.output.offset, stop - start)
                }),
                readable: range.readable,
            }
        })
        .collect()
}

#[test]
fn a_plan_reader_reads_any_offset_as_materialize_writes_it() {
    let image = fixtures::build_volume();
    let source = Failing {
        inner: Bytes(image),
        bad: vec![Extent::new(position(31) + 1000, 1)],
    };
    let adapter = ExfatSalvage::probe(&source);
    let mut plan = Plan::new(vec![
        PlanStep::Bytes(b"head".to_vec()),
        clusters(30, 2),
        PlanStep::Zeros(100),
        PlanStep::Extent(Extent::new(position(40) + 3, 5000)),
        clusters(33, 1),
    ]);
    let total = 4 + 2 * C + 100 + 5000 + C;
    plan.length = Some(total - 10);
    plan.valid_length = Some(total - 50);
    let mut output = Vec::new();
    let report = adapter.materialize(&source, &plan, &mut output).unwrap();
    assert_eq!(
        report.unreadable().collect::<Vec<_>>(),
        [Extent::new(4 + C + 512, 512)]
    );

    let reader = adapter.plan_reader(&source, &plan).unwrap();
    assert_eq!(reader.len(), total - 10);
    assert_eq!(reader.plan(), &plan);
    let mut offsets = vec![0, 4 + C + 700, 4 + C + 1100, total - 50, total - 11];
    for boundary in [4, 4 + 2 * C, 4 + 2 * C + 100, 4 + 2 * C + 5100] {
        offsets.extend([boundary - 1, boundary, boundary + 1]);
    }
    for offset in offsets {
        for size in [1, 7, 600, 9000] {
            let mut buffer = vec![0xAA; size];
            let read = reader.read_at(offset, &mut buffer).unwrap();
            let length = (reader.len() - offset).min(size as u64);
            assert_eq!((read.offset, read.length), (offset, length));
            let span = offset as usize..(offset + length) as usize;
            assert_eq!(buffer[..length as usize], output[span], "{offset}+{size}");
            assert_eq!(
                read.ranges,
                clipped(&report.ranges, offset, length),
                "{offset}+{size}"
            );
            assert_eq!(reader.source_of(offset), report.source_of(offset));
        }
    }
    let unreadable = reader.read_at(4 + C + 1000, &mut [0; 4]).unwrap();
    assert_eq!(
        unreadable.unreadable().collect::<Vec<_>>(),
        [Extent::new(4 + C + 1000, 4)]
    );
    assert_eq!(reader.source_of(total - 50), None);
    assert_eq!(reader.source_of(4 + 2 * C), None);

    let end = reader.read_at(reader.len(), &mut [0; 8]).unwrap();
    assert_eq!((end.length, end.ranges.len()), (0, 0));
    assert!(matches!(
        reader.read_at(reader.len() + 1, &mut [0; 8]),
        Err(SalvageError::Bounds)
    ));
    assert_eq!(reader.source_of(reader.len()), None);

    // Opening a reader checks the plan as materializing it does.
    plan.valid_length = Some(total);
    assert!(matches!(
        adapter.plan_reader(&source, &plan),
        Err(SalvageError::Plan(PlanFault::ValidLengthPastEnd { .. }))
    ));
}

/// A source that records every read it is asked for.
struct Recording {
    inner: Bytes,
    reads: std::sync::Mutex<Vec<Extent>>,
}

impl BlockRead for Recording {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(Extent::new(offset, output.len() as u64));
        self.inner.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.inner.size_bytes()
    }
}

#[test]
fn a_plan_reader_reads_only_the_source_bytes_a_request_covers() {
    let source = Recording {
        inner: Bytes(fixtures::build_volume()),
        reads: Default::default(),
    };
    let adapter = ExfatSalvage::probe(&source);
    let mut plan = Plan::new(vec![clusters(30, 1), clusters(40, 1), clusters(60, 1)]);
    plan.valid_length = Some(2 * C + 10);
    source.reads.lock().unwrap().clear();
    let reader = adapter.plan_reader(&source, &plan).unwrap();
    assert!(
        source.reads.lock().unwrap().is_empty(),
        "opening reads nothing"
    );

    let inside = |reads: &[Extent], clusters: &[u32]| {
        reads.iter().all(|read| {
            clusters.iter().any(|cluster| {
                position(*cluster) <= read.offset && read.end().unwrap() <= position(*cluster) + C
            })
        })
    };
    let mut buffer = [0; 10];
    reader.read_at(C + 100, &mut buffer).unwrap();
    let reads = std::mem::take(&mut *source.reads.lock().unwrap());
    assert_eq!(reads, [Extent::new(position(40), 512)]);

    let mut buffer = [0; 30];
    let read = reader.read_at(2 * C - 5, &mut buffer).unwrap();
    let reads = std::mem::take(&mut *source.reads.lock().unwrap());
    assert!(!reads.is_empty() && inside(&reads, &[40, 60]), "{reads:?}");
    assert!(reads
        .iter()
        .all(|read| read.offset >= position(40) + C - 512));
    assert_eq!(
        read.ranges.len(),
        3,
        "cluster 40, cluster 60, zeros past the valid length"
    );

    // Bytes past the valid length are zeros that need no read.
    reader.read_at(2 * C + 10, &mut [0xFF; 100]).unwrap();
    assert!(source.reads.lock().unwrap().is_empty());
}

/// A fixed plan whose canonical bytes were written out by hand from the
/// documented layout, outside the crate, and whose digest is macOS
/// `shasum -a 256` over those bytes.
fn pinned() -> Plan {
    let mut plan = Plan::new(vec![
        PlanStep::Extent(Extent::new(28672, 4)),
        PlanStep::Zeros(2),
        PlanStep::Bytes(vec![1, 2]),
        clusters(70, 1),
    ]);
    plan.length = Some(4100);
    plan.valid_length = Some(4000);
    plan.metadata = PlanMetadata {
        name: Some("short.bin".into()),
        created: Some("2024-01-02 03:04:05 +00:00".into()),
        modified: None,
        accessed: Some(String::new()),
    };
    plan
}

const PINNED_BYTES: &str = "66732d73616c766167652d706c616e01040000000000000001007000000000000004000000000000000202000000000000000302000000000000000102044600000000000000010000000000000001041000000000000001a00f00000000000001090000000000000073686f72742e62696e011a00000000000000323032342d30312d30322030333a30343a3035202b30303a303000010000000000000000";
const PINNED_DIGEST: &str = "68fd79115f94e496050ab614994f863e4e7ce56ae2aa38f4dea80193799ff020";

#[test]
fn a_plan_digest_is_pinned_over_its_documented_encoding() {
    let plan = pinned();
    let hex: String = plan
        .canonical_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(hex, PINNED_BYTES);
    assert_eq!(plan.digest(), PINNED_DIGEST);
    let back: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
    assert_eq!(back.digest(), PINNED_DIGEST);
    assert_eq!(
        Plan::new(Vec::new()).digest(),
        Plan::default().digest(),
        "an empty plan has one identity"
    );
}

#[test]
fn any_change_to_a_step_length_or_metadata_changes_the_digest() {
    let changes: [fn(&mut Plan); 24] = [
        |plan| plan.steps[0] = PlanStep::Extent(Extent::new(28673, 4)),
        |plan| plan.steps[0] = PlanStep::Extent(Extent::new(28672, 5)),
        |plan| plan.steps[1] = PlanStep::Zeros(3),
        |plan| plan.steps[2] = PlanStep::Bytes(vec![1, 3]),
        |plan| plan.steps[3] = clusters(71, 1),
        |plan| plan.steps[3] = clusters(70, 2),
        // The same output bytes, from a different plan.
        |plan| plan.steps[1] = PlanStep::Bytes(vec![0, 0]),
        |plan| {
            plan.steps
                .splice(2..3, [PlanStep::Bytes(vec![1]), PlanStep::Bytes(vec![2])]);
        },
        |plan| plan.steps.swap(0, 1),
        |plan| {
            plan.steps.pop();
        },
        |plan| plan.steps.push(PlanStep::Zeros(0)),
        |plan| plan.length = None,
        |plan| plan.length = Some(4101),
        |plan| plan.valid_length = None,
        |plan| plan.valid_length = Some(4001),
        |plan| plan.metadata.name = None,
        |plan| plan.metadata.name = Some("short.bim".into()),
        |plan| plan.metadata.created = None,
        |plan| plan.metadata.modified = Some(String::new()),
        |plan| plan.metadata.accessed = None,
        |plan| plan.metadata.accessed = Some(" ".into()),
        // The same text under a different field.
        |plan| {
            plan.metadata.modified = plan.metadata.created.take();
        },
        |plan| {
            plan.metadata.created = plan.metadata.name.clone();
            plan.metadata.name = None;
        },
        |plan| plan.length = plan.valid_length,
    ];
    let mut seen = std::collections::HashSet::from([PINNED_DIGEST.to_owned()]);
    for (index, change) in changes.iter().enumerate() {
        let mut plan = pinned();
        change(&mut plan);
        assert_ne!(plan, pinned(), "change {index}");
        assert!(seen.insert(plan.digest()), "change {index} kept a digest");
    }
}

#[test]
fn one_plan_materialized_twice_gives_identical_bytes_and_reports() {
    let image = fixtures::build_volume();
    let source = Failing {
        inner: Bytes(image),
        bad: vec![Extent::new(position(31) + 700, 3)],
    };
    let mut plan = Plan::new(vec![
        PlanStep::Bytes(b"head".to_vec()),
        clusters(30, 2),
        PlanStep::Zeros(5),
        PlanStep::Extent(Extent::new(position(40) + 7, 3000)),
        clusters(33, 1),
    ]);
    plan.length = Some(4 + 2 * C + 5 + 3000 + C - 9);
    plan.valid_length = Some(4 + 2 * C + 5 + 2000);
    let digest = plan.digest();

    let run = |plan: &Plan| {
        let adapter = ExfatSalvage::probe(&source);
        let mut output = Vec::new();
        let report = adapter.materialize(&source, plan, &mut output).unwrap();
        (output, report)
    };
    let first = run(&plan);
    assert_eq!(first.1.unreadable().count(), 1);
    let second = run(&plan);
    assert_eq!(first, second);
    let json = serde_json::to_string(&plan).unwrap();
    let again: Plan = serde_json::from_str(&json).unwrap();
    assert_eq!(again.digest(), digest);
    assert_eq!(run(&again), first);
}
