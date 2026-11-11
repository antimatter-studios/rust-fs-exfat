//! The driver conformance set: checks that any driver runs against its own
//! salvage adapter and strict API.
//!
//! The driver convention (`docs/convention.md`, section 6) asks every driver
//! to run one shared test set. This module is that set, and it names no
//! format. A driver describes itself through [`Driver`]: how its salvage
//! adapter is built for a source, how its strict API reads a whole source, one
//! healthy image, and damaged images with the evidence each must leave.
//! [`run`] checks five rules against it and returns a [`Report`]:
//!
//! | Rule | What is checked |
//! |---|---|
//! | [`Rule::Consistency`] | Scans count what they stream, resume where they stop, find each candidate again over its own record and keep its identity unique; walks stay on the source, merge adjacent extents and map no more than they claim; trees hold one node per candidate in input order, with the same parents in any order; damage reports count and stop cleanly; every verb answers the same twice; reads by extent and materialized plans return the source's own bytes with exact provenance; cluster steps give what their mapped extents give, cut by a final length and zeroed past a valid length, and a cluster outside the layout is refused. |
//! | [`Rule::ReadOnly`] | Every verb is called with a source type that has no write path, so a trait or adapter needing one does not compile; an adapter is built from a read view only; an adapter built from one source decodes nothing from another; materialization writes only to the caller's sink. |
//! | [`Rule::Reported`] | Each damaged image shows its declared [`Evidence`], and no verb refuses it except as the trait documents; unreadable bytes are listed, flagged or reported; the healthy image shows none of that evidence. |
//! | [`Rule::StrictRejects`] | The strict API accepts the healthy image and rejects every damaged one, and a source it cannot read at all. |
//! | [`Rule::Capabilities`] | A missing feature is refused with `Unsupported` naming it, never answered with an empty result; a supported feature is never refused and is exercised at least once, deleted candidates and their extents included. |
//!
//! Every check reads through an in-memory source that implements only
//! `BlockRead`. Its reads fail wherever they touch a range a case marks
//! unreadable, widened to whole 512-byte sectors as on failing media.
//!
//! Images come from the driver, because only the driver can encode its
//! format. From the healthy image the set also derives two sources of its own:
//! one whose every read fails, and one whose first candidate's record cannot
//! be read.
//!
//! On damaged input a walk or a tree may refuse with `NoGeometry` only when the
//! damage report holds an error-severity finding that explains it, and a read
//! by extent may fail only over unreadable bytes. Any other refusal is the
//! salvage path rejecting damage.
//!
//! A rule that ran no checks fails the report: a set that quietly declines to
//! run reads exactly like one that passes. A driver runs it from a test:
//!
//! ```text
//! struct Mine;
//! impl Driver for Mine { /* adapter, strict API, images */ }
//!
//! #[test]
//! fn the_driver_conforms() {
//!     conformance::run(&Mine).assert_conforms();
//! }
//! ```

use crate::salvage::{
    Candidate, CandidateState, Capabilities, ClusterLayout, Damage, DamageKind, DamageReport,
    Extent, ExtentWalk, Feature, Integrity, MaterializeReport, MaterializedRange, Parent, Plan,
    PlanFault, PlanRead, PlanReader, PlanStep, SalvageAdapter, SalvageError, ScanReport, Severity,
    Sink, Tree, WalkEnd,
};
use fs_core::BlockRead;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::ops::ControlFlow;

/// Candidates per source whose stops, resumes and own-record scans are checked.
const SAMPLE: usize = 32;
/// Candidates walked per source.
const WALKS: usize = 256;
/// Extents read back per source.
const READS: usize = 64;
/// Bytes read back per extent.
const READ_BYTES: u64 = 64 * 1024;
/// Source bytes a materialization plan copies from walked extents.
const PLAN_BYTES: u64 = 1024 * 1024;
/// Walked extents a materialization plan copies.
const PLAN_EXTENTS: usize = 8;
/// Bytes per read when a plan's output is read back in pieces: odd, so reads
/// straddle sector and step boundaries.
const READ_PIECE: usize = 4093;
/// The length of the probe extent every source is read and materialized at.
const PROBE_BYTES: u64 = 4096;
/// The granularity unreadable ranges fail at.
const SECTOR: u64 = 512;
/// Violations a displayed report lists before counting the rest.
const SHOWN: usize = 40;

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

/// What a driver hands the conformance set: its salvage adapter, its strict
/// API and its test images.
pub trait Driver {
    /// The driver's salvage adapter. Its candidate detail must compare, so
    /// the set can tell whether two answers agree.
    type Adapter: SalvageAdapter<Detail: PartialEq>;
    /// What the strict API returns when it rejects a source.
    type StrictError: fmt::Debug;

    /// Builds the salvage adapter for a source, the way the driver's callers
    /// do. It is given only a read view, so an adapter that needs a writable
    /// device cannot be plugged in.
    fn salvage(&self, source: &dyn BlockRead) -> Self::Adapter;

    /// Runs the strict API over the whole source and returns `Err` when it
    /// rejects it. A filesystem driver opens the volume and reads every
    /// directory and file it presents; a block device driver opens the
    /// container and reads the whole virtual disk.
    fn strict(&self, source: &dyn BlockRead) -> Result<(), Self::StrictError>;

    /// An image the strict API accepts and the salvage adapter finds clean.
    /// When the adapter supports deleted candidates, it should hold one, or
    /// a damaged image should, so that the capability is exercised.
    fn healthy(&self) -> Vec<u8>;

    /// Damaged images. Each must show its declared evidence on the salvage
    /// path, and the strict API must reject each one.
    fn damaged(&self) -> Vec<Damaged>;
}

/// One damaged image and the evidence its damage must leave.
#[derive(Clone, Debug)]
pub struct Damaged {
    /// Names the case in violations.
    pub name: String,
    pub image: Vec<u8>,
    /// Byte ranges whose reads fail, widened to whole sectors.
    pub unreadable: Vec<Extent>,
    pub evidence: Evidence,
}

impl Damaged {
    pub fn new(name: impl Into<String>, image: Vec<u8>, evidence: Evidence) -> Self {
        Self {
            name: name.into(),
            image,
            unreadable: Vec::new(),
            evidence,
        }
    }

    /// Makes reads that touch `extent` fail, as on failing media.
    pub fn unreadable(mut self, extent: Extent) -> Self {
        self.unreadable.push(extent);
        self
    }
}

/// Where damage must show on the salvage path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// The damage report streams a finding of this kind.
    Damage(DamageKind),
    /// A scan returns a candidate whose own integrity check failed.
    FailedCandidate,
    /// A live candidate's walk ends before its claim is covered.
    BrokenWalk,
}

/// The five rules of the conformance set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// Adapter verbs return consistent evidence.
    Consistency,
    /// The salvage path is read-only by type.
    ReadOnly,
    /// Damaged input is reported, not rejected, on the salvage path.
    Reported,
    /// The strict API rejects the same damaged input.
    StrictRejects,
    /// Capability flags match behaviour.
    Capabilities,
}

impl Rule {
    pub const ALL: [Rule; 5] = [
        Rule::Consistency,
        Rule::ReadOnly,
        Rule::Reported,
        Rule::StrictRejects,
        Rule::Capabilities,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Consistency => "consistent evidence",
            Self::ReadOnly => "read-only by type",
            Self::Reported => "damage reported, not rejected",
            Self::StrictRejects => "strict API rejects damage",
            Self::Capabilities => "capabilities match behaviour",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A failed check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub rule: Rule,
    /// The image or derived source the check ran on.
    pub case: String,
    pub detail: String,
}

/// What a run checked and what failed.
#[derive(Clone, Debug, Default)]
pub struct Report {
    checks: [u64; Rule::ALL.len()],
    violations: Vec<Violation>,
}

impl Report {
    /// Checks run under a rule, passed or not.
    pub fn checks(&self, rule: Rule) -> u64 {
        self.checks[rule as usize]
    }

    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// True when every rule ran at least one check and none failed.
    pub fn conforms(&self) -> bool {
        self.violations.is_empty() && Rule::ALL.iter().all(|rule| self.checks(*rule) > 0)
    }

    /// Panics with every violation, and every rule that ran nothing, unless
    /// the driver conforms.
    #[track_caller]
    pub fn assert_conforms(&self) {
        assert!(self.conforms(), "{self}");
    }

    fn record(&mut self, rule: Rule, case: &str, passed: bool, detail: impl FnOnce() -> String) {
        self.checks[rule as usize] += 1;
        if !passed {
            self.violations.push(Violation {
                rule,
                case: case.to_owned(),
                detail: detail(),
            });
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "conformance: {} violation(s)",
            self.violations.len()
        )?;
        for rule in Rule::ALL {
            let checks = self.checks(rule);
            let note = if checks == 0 {
                ", so it proves nothing"
            } else {
                ""
            };
            writeln!(formatter, "  {rule}: {checks} check(s){note}")?;
        }
        for violation in self.violations.iter().take(SHOWN) {
            writeln!(
                formatter,
                "  [{}] {}: {}",
                violation.rule, violation.case, violation.detail
            )?;
        }
        if let Some(hidden) = self
            .violations
            .len()
            .checked_sub(SHOWN)
            .filter(|hidden| *hidden > 0)
        {
            writeln!(
                formatter,
                "  and {hidden} more; Report::violations lists them all"
            )?;
        }
        Ok(())
    }
}

/// Runs the conformance set against a driver.
pub fn run<D: Driver>(driver: &D) -> Report {
    let mut run = Run {
        driver,
        report: Report::default(),
        exercised: HashSet::new(),
        probed: HashSet::new(),
    };
    run.types();
    let healthy = Image::new(driver.healthy(), &[]);
    let observed = run.examine("healthy", Input::Healthy, &healthy);
    run.healthy(&healthy, &observed);
    let cases = driver.damaged();
    run.check(Rule::Reported, "damaged", !cases.is_empty(), || {
        "the driver supplies no damaged images".into()
    });
    for case in cases {
        let image = Image::new(case.image, &case.unreadable);
        let seen = run.examine(&case.name, Input::Damaged, &image);
        run.damaged(&case.name, &image, case.evidence, &seen);
    }
    run.unreadable(&healthy, &observed);
    run.exercised(&observed.capabilities);
    run.report
}

type Detail<D> = <<D as Driver>::Adapter as SalvageAdapter>::Detail;

/// Whether a source holds damage. Verbs may refuse only damaged input, and
/// only for the reasons the trait documents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    Healthy,
    Damaged,
}

impl Input {
    /// The rule a failing verb breaks: refusing damaged input is the salvage
    /// path rejecting damage.
    fn fault(self) -> Rule {
        match self {
            Self::Healthy => Rule::Consistency,
            Self::Damaged => Rule::Reported,
        }
    }
}

/// One source under examination, with the adapter built for it.
struct Subject<'a, A> {
    case: &'a str,
    input: Input,
    image: &'a Image,
    adapter: A,
    capabilities: Capabilities,
}

/// What one source yielded, for the checks that compare sources.
struct Observed<T> {
    capabilities: Capabilities,
    candidates: Vec<Candidate<T>>,
    /// `None` when the report was refused or failed.
    findings: Option<Vec<Damage>>,
    walks: Vec<(CandidateState, ExtentWalk)>,
}

struct Run<'d, D> {
    driver: &'d D,
    report: Report,
    /// Supported features some check used successfully.
    exercised: HashSet<Feature>,
    /// Missing features some check probed for a refusal.
    probed: HashSet<Feature>,
}

impl<D: Driver> Run<'_, D> {
    fn check(&mut self, rule: Rule, case: &str, passed: bool, detail: impl FnOnce() -> String) {
        self.report.record(rule, case, passed, detail);
    }

    /// The compile-time half of the read-only and plain-data rules. Each
    /// verb must take its source as `&dyn BlockRead` and materialization
    /// its output as `io::Write`; evidence and plans must serialize and
    /// deserialize without borrowing anything.
    #[allow(clippy::type_complexity)]
    fn types(&mut self) {
        type Adapter<D> = <D as Driver>::Adapter;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            Extent,
            &mut dyn Sink<Candidate<Detail<D>>>,
        ) -> Result<ScanReport, SalvageError> = Adapter::<D>::find_candidates;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            &Candidate<Detail<D>>,
        ) -> Result<ExtentWalk, SalvageError> = Adapter::<D>::walk_extents;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            &[Candidate<Detail<D>>],
        ) -> Result<Tree, SalvageError> = Adapter::<D>::rebuild_tree;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            &mut dyn Sink<Damage>,
        ) -> Result<DamageReport, SalvageError> = Adapter::<D>::report_damage;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            Extent,
            u64,
            &mut [u8],
        ) -> Result<usize, SalvageError> = Adapter::<D>::read_extent;
        let _: fn(
            &Adapter<D>,
            &dyn BlockRead,
            &Plan,
            &mut dyn io::Write,
        ) -> Result<MaterializeReport, SalvageError> = Adapter::<D>::materialize;
        let _: fn(&Adapter<D>) -> Result<ClusterLayout, SalvageError> =
            Adapter::<D>::cluster_layout;
        let _: for<'a> fn(
            &'a Adapter<D>,
            &'a dyn BlockRead,
            &'a Plan,
        ) -> Result<PlanReader<'a, Adapter<D>>, SalvageError> = Adapter::<D>::plan_reader;
        let _: fn(&D, &dyn BlockRead) -> Adapter<D> = D::salvage;
        self.check(Rule::ReadOnly, "types", true, String::new);

        plain_data::<Candidate<Detail<D>>>();
        plain_data::<ScanReport>();
        plain_data::<ExtentWalk>();
        plain_data::<Tree>();
        plain_data::<Damage>();
        plain_data::<DamageReport>();
        plain_data::<Plan>();
        plain_data::<MaterializeReport>();
        plain_data::<PlanRead>();
        plain_data::<Capabilities>();
        self.check(Rule::Consistency, "types", true, String::new);
    }

    /// Runs every verb over one source, checking each answer and how the
    /// answers agree.
    fn examine(&mut self, case: &str, input: Input, image: &Image) -> Observed<Detail<D>> {
        let adapter = self.driver.salvage(image);
        let subject = Subject {
            case,
            input,
            image,
            capabilities: adapter.capabilities(),
            adapter,
        };
        self.capabilities(&subject);
        let findings = self.damage(&subject);
        let explained = input == Input::Damaged
            && findings.as_ref().is_some_and(|findings| {
                findings
                    .iter()
                    .any(|finding| finding.severity == Severity::Error)
            });
        let candidates = self.scan(&subject);
        let walks = self.walks(&subject, &candidates, explained);
        self.tree(&subject, &candidates, explained);
        self.reads(&subject);
        self.materialize(&subject, &walks);
        self.clusters(&subject, explained);
        Observed {
            capabilities: subject.capabilities,
            candidates,
            findings,
            walks,
        }
    }

    fn capabilities(&mut self, subject: &Subject<'_, D::Adapter>) {
        let missing = &subject.capabilities.missing;
        let unique = missing
            .iter()
            .enumerate()
            .all(|(index, feature)| !missing[..index].contains(feature));
        self.check(Rule::Capabilities, subject.case, unique, || {
            format!("capabilities list a feature twice: {missing:?}")
        });
    }

    /// Checks that a verb the adapter lists as missing was refused with
    /// `Unsupported` naming the feature, not answered.
    fn refused<T>(&mut self, case: &str, feature: Feature, result: &Result<T, SalvageError>) {
        self.probed.insert(feature);
        let named =
            matches!(result, Err(SalvageError::Unsupported(refused)) if *refused == feature);
        self.check(Rule::Capabilities, case, named, || {
            let answer = match result {
                Ok(_) => "answered".to_owned(),
                Err(error) => format!("failed with \"{error}\""),
            };
            format!("{feature} is listed as missing, yet the verb {answer}")
        });
    }

    /// Sorts the failure of a supported verb: a refusal naming a feature, an
    /// allowed refusal of damaged input, or a fault.
    fn failed(&mut self, subject: &Subject<'_, D::Adapter>, verb: &str, error: &SalvageError) {
        let case = subject.case;
        if let SalvageError::Unsupported(feature) = error {
            self.check(Rule::Capabilities, case, false, || {
                format!("{verb} refused with Unsupported({feature}), a feature the adapter does not list as missing")
            });
        } else {
            self.check(subject.input.fault(), case, false, || {
                format!("{verb} failed: {error}")
            });
        }
    }

    /// Like [`Self::failed`], for verbs that map storage claims: on damaged
    /// input explained by an error-severity finding, they may refuse because
    /// no layout copy is usable.
    fn failed_mapping(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        verb: &str,
        error: &SalvageError,
        explained: bool,
    ) {
        if explained && matches!(error, SalvageError::NoGeometry) {
            self.check(Rule::Reported, subject.case, true, String::new);
        } else {
            self.failed(subject, verb, error);
        }
    }

    fn damage(&mut self, subject: &Subject<'_, D::Adapter>) -> Option<Vec<Damage>> {
        let Subject {
            case,
            image,
            adapter,
            ..
        } = subject;
        let mut findings = Vec::new();
        let result = adapter.report_damage(*image, &mut findings);
        if !subject.capabilities.supports(Feature::ReportDamage) {
            self.refused(case, Feature::ReportDamage, &result);
            return None;
        }
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                self.failed(subject, "report_damage", &error);
                return None;
            }
        };
        let streamed = findings.len() as u64;
        self.check(
            Rule::Consistency,
            case,
            report.reported == streamed && !report.stopped,
            || {
                format!(
                    "report_damage streamed {streamed} finding(s), and its report says {report:?}"
                )
            },
        );
        let malformed: Vec<_> = findings
            .iter()
            .filter(|finding| finding.structure.is_empty() || finding.location.end().is_none())
            .collect();
        self.check(Rule::Consistency, case, malformed.is_empty(), || {
            format!("findings name no structure or an unrepresentable location: {malformed:?}")
        });
        let mut again = Vec::new();
        let repeat = adapter.report_damage(*image, &mut again);
        self.check(
            Rule::Consistency,
            case,
            matches!(&repeat, Ok(second) if *second == report) && again == findings,
            || "a second damage report differs from the first".into(),
        );
        for count in sample(findings.len()).into_iter().map(|index| index + 1) {
            let mut seen = Vec::new();
            let mut stop = |finding| {
                seen.push(finding);
                if seen.len() == count {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            let result = adapter.report_damage(*image, &mut stop);
            let early = count < findings.len();
            let clean = matches!(&result, Ok(stopped) if stopped.reported == count as u64 && (stopped.stopped || !early))
                && seen[..] == findings[..count];
            self.check(Rule::Consistency, case, clean, || {
                format!("a damage report stopped after {count} finding(s) did not stream exactly the first {count} and say it stopped: {result:?}")
            });
        }
        if !findings.is_empty() {
            self.exercised.insert(Feature::ReportDamage);
        }
        Some(findings)
    }

    fn scan(&mut self, subject: &Subject<'_, D::Adapter>) -> Vec<Candidate<Detail<D>>> {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let size = image.size();
        let whole = Extent::new(0, size);
        let mut found = Vec::new();
        let result = adapter.find_candidates(*image, whole, &mut found);
        if !capabilities.supports(Feature::FindCandidates) {
            self.refused(case, Feature::FindCandidates, &result);
            return Vec::new();
        }
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                self.failed(subject, "find_candidates", &error);
                return Vec::new();
            }
        };
        let streamed = found.len() as u64;
        self.check(
            Rule::Consistency,
            case,
            report.candidates == streamed && report.stopped_at.is_none(),
            || format!("a scan streamed {streamed} candidate(s), and its report says {report:?}"),
        );
        self.scan_evidence(subject, whole, &report, &found);

        let mut offsets: Vec<u64> = found
            .iter()
            .map(|candidate| candidate.location.offset)
            .collect();
        offsets.sort_unstable();
        let shared: Vec<_> = offsets
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
            .collect();
        self.check(Rule::Consistency, case, shared.is_empty(), || {
            format!("candidates share the location offsets that identify them: {shared:?}")
        });

        let deleted = found
            .iter()
            .filter(|candidate| candidate.state == CandidateState::Deleted)
            .count();
        if capabilities.supports(Feature::DeletedCandidates) {
            if deleted > 0 {
                self.exercised.insert(Feature::DeletedCandidates);
            }
        } else {
            self.probed.insert(Feature::DeletedCandidates);
            self.check(Rule::Capabilities, case, deleted == 0, || {
                format!(
                    "{} is listed as missing, yet a scan returned {deleted} deleted candidate(s)",
                    Feature::DeletedCandidates
                )
            });
        }
        if !found.is_empty() {
            self.exercised.insert(Feature::FindCandidates);
        }

        let mut again = Vec::new();
        let repeat = adapter.find_candidates(*image, whole, &mut again);
        self.check(
            Rule::Consistency,
            case,
            matches!(&repeat, Ok(second) if *second == report) && again == found,
            || "a second scan differs from the first".into(),
        );
        for index in sample(found.len()) {
            self.resume(subject, &found, index);
            self.rescan(subject, &found[index]);
        }
        for range in [Extent::new(size, 1), Extent::new(u64::MAX, 2)] {
            let result = adapter.find_candidates(*image, range, &mut Vec::new());
            self.check(
                Rule::Consistency,
                case,
                matches!(result, Err(SalvageError::Bounds)),
                || format!("a scan of {range:?}, outside the source, was not refused as out of bounds: {result:?}"),
            );
        }
        let mut none = Vec::new();
        let result = adapter.find_candidates(*image, Extent::new(size, 0), &mut none);
        self.check(
            Rule::Consistency,
            case,
            matches!(&result, Ok(report) if report.candidates == 0) && none.is_empty(),
            || format!("an empty scan at the end of the source found something: {result:?}"),
        );
        found
    }

    /// Checks what one scan says against the range it covered.
    fn scan_evidence(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        range: Extent,
        report: &ScanReport,
        found: &[Candidate<Detail<D>>],
    ) {
        let case = subject.case;
        let size = subject.image.size();
        let range_end = range.end().unwrap_or(u64::MAX);
        let outside: Vec<_> = found
            .iter()
            .map(|candidate| candidate.location)
            .filter(|location| {
                location.length == 0
                    || location.end().is_none_or(|end| end > size)
                    || location.offset < range.offset
                    || location.offset >= range_end
            })
            .collect();
        self.check(Rule::Consistency, case, outside.is_empty(), || {
            format!("records that are empty, leave the source or start outside the scan of {range:?}: {outside:?}")
        });
        let stray: Vec<_> = report
            .unreadable
            .iter()
            .filter(|listed| {
                listed.length == 0
                    || listed.offset < range.offset
                    || listed.end().is_none_or(|end| end > range_end)
            })
            .collect();
        self.check(Rule::Consistency, case, stray.is_empty(), || {
            format!("unreadable ranges that are empty or outside the scan of {range:?}: {stray:?}")
        });
        let over: Vec<_> = found
            .iter()
            .map(|candidate| candidate.location)
            .filter(|location| {
                report
                    .unreadable
                    .iter()
                    .any(|listed| overlaps(*listed, *location))
            })
            .collect();
        self.check(Rule::Consistency, case, over.is_empty(), || {
            format!(
                "candidates whose records overlap bytes the scan listed as unreadable: {over:?}"
            )
        });
        if subject.image.unreadable.is_empty() {
            self.check(
                Rule::Consistency,
                case,
                report.unreadable.is_empty(),
                || {
                    format!(
                        "a scan of a source that reads everywhere listed unreadable bytes: {:?}",
                        report.unreadable
                    )
                },
            );
        }
    }

    /// Stops a scan after one candidate and resumes it where the report says.
    fn resume(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        found: &[Candidate<Detail<D>>],
        index: usize,
    ) {
        let Subject {
            case,
            image,
            adapter,
            ..
        } = subject;
        let size = image.size();
        let mut first = Vec::new();
        let mut take = |candidate| {
            first.push(candidate);
            if first.len() == index + 1 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let result = adapter.find_candidates(*image, Extent::new(0, size), &mut take);
        let offset = found[index].location.offset;
        let resume = match &result {
            Ok(report) if report.candidates == index as u64 + 1 => report
                .stopped_at
                .filter(|resume| *resume > offset && *resume <= size),
            _ => None,
        };
        let Some(resume) = resume else {
            self.check(Rule::Consistency, case, false, || {
                format!("a scan stopped after candidate {index} did not say where to resume: {result:?}")
            });
            return;
        };
        self.check(Rule::Consistency, case, first[..] == found[..=index], || {
            format!("a scan stopped after candidate {index} streamed other candidates than the whole scan")
        });
        let rest_range = Extent::new(resume, size - resume);
        let mut rest = Vec::new();
        let result = adapter.find_candidates(*image, rest_range, &mut rest);
        let same = result.is_ok() && rest[..] == found[index + 1..];
        self.check(Rule::Consistency, case, same, || {
            format!("resuming at byte {resume} after candidate {index} found other candidates than the whole scan: {result:?}")
        });
        if let Ok(report) = &result {
            self.scan_evidence(subject, rest_range, report, &rest);
        }
    }

    /// Scans a candidate's own record, which must find it again first.
    fn rescan(&mut self, subject: &Subject<'_, D::Adapter>, candidate: &Candidate<Detail<D>>) {
        let mut found = Vec::new();
        let result = subject
            .adapter
            .find_candidates(subject.image, candidate.location, &mut found);
        let again = result.is_ok() && found.first() == Some(candidate);
        self.check(Rule::Consistency, subject.case, again, || {
            format!(
                "a scan of the record at {:?} did not find its candidate again: {result:?}",
                candidate.location
            )
        });
        if let Ok(report) = &result {
            self.scan_evidence(subject, candidate.location, report, &found);
        }
    }

    fn walks(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        candidates: &[Candidate<Detail<D>>],
        explained: bool,
    ) -> Vec<(CandidateState, ExtentWalk)> {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let mut walks = Vec::new();
        if candidates.is_empty() {
            return walks;
        }
        let rebuilt = self.driver.salvage(*image);
        let mut reads = 0;
        for candidate in candidates.iter().take(WALKS) {
            let result = adapter.walk_extents(*image, candidate);
            let deleted = candidate.state == CandidateState::Deleted;
            if !capabilities.supports(Feature::WalkExtents) {
                self.refused(case, Feature::WalkExtents, &result);
                continue;
            }
            if deleted && !capabilities.supports(Feature::DeletedExtents) {
                self.refused(case, Feature::DeletedExtents, &result);
                continue;
            }
            let walk = match result {
                Ok(walk) => walk,
                Err(error) => {
                    self.failed_mapping(subject, "walk_extents", &error, explained);
                    continue;
                }
            };
            let at = candidate.location.offset;
            self.walk_shape(subject, at, &walk);
            let again = adapter.walk_extents(*image, candidate);
            self.check(
                Rule::Consistency,
                case,
                again.as_ref().ok() == Some(&walk),
                || format!("a second walk of the candidate at byte {at} differs: {again:?}"),
            );
            let fresh = rebuilt.walk_extents(*image, candidate);
            self.check(Rule::Consistency, case, fresh.as_ref().ok() == Some(&walk), || {
                format!("an adapter built again for the same source walks the candidate at byte {at} differently: {fresh:?}")
            });
            if !walk.extents.is_empty() {
                self.exercised.insert(Feature::WalkExtents);
                if deleted {
                    self.exercised.insert(Feature::DeletedExtents);
                }
            }
            for extent in &walk.extents {
                if reads < READS {
                    self.read_back(subject, *extent);
                    reads += 1;
                }
            }
            walks.push((candidate.state, walk));
        }
        if capabilities.supports(Feature::WalkExtents)
            && !capabilities.supports(Feature::DeletedExtents)
        {
            if let Some(live) = candidates
                .iter()
                .find(|candidate| candidate.state != CandidateState::Deleted)
            {
                let mut deleted = live.clone();
                deleted.state = CandidateState::Deleted;
                let result = adapter.walk_extents(*image, &deleted);
                self.refused(case, Feature::DeletedExtents, &result);
            }
        }
        walks
    }

    fn walk_shape(&mut self, subject: &Subject<'_, D::Adapter>, at: u64, walk: &ExtentWalk) {
        let case = subject.case;
        let size = subject.image.size();
        let off_source: Vec<_> = walk
            .extents
            .iter()
            .filter(|extent| extent.length == 0 || extent.end().is_none_or(|end| end > size))
            .collect();
        self.check(Rule::Consistency, case, off_source.is_empty(), || {
            format!("the walk of the candidate at byte {at} names empty extents or extents off the source: {off_source:?}")
        });
        let unmerged = walk
            .extents
            .windows(2)
            .any(|pair| pair[0].end() == Some(pair[1].offset));
        self.check(Rule::Consistency, case, !unmerged, || {
            format!(
                "the walk of the candidate at byte {at} leaves adjacent extents unmerged: {:?}",
                walk.extents
            )
        });
        let mapped = walk
            .extents
            .iter()
            .try_fold(0u64, |total, extent| total.checked_add(extent.length));
        let fits = mapped.is_some_and(|mapped| {
            mapped <= walk.claimed && (walk.end != WalkEnd::Complete || mapped == walk.claimed)
        });
        self.check(Rule::Consistency, case, fits, || {
            format!(
                "the walk of the candidate at byte {at} maps {mapped:?} of {} claimed bytes and ends {:?}",
                walk.claimed, walk.end
            )
        });
    }

    fn tree(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        candidates: &[Candidate<Detail<D>>],
        explained: bool,
    ) {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let result = adapter.rebuild_tree(*image, candidates);
        if !capabilities.supports(Feature::RebuildTree) {
            self.refused(case, Feature::RebuildTree, &result);
            return;
        }
        let tree = match result {
            Ok(tree) => tree,
            Err(error) => {
                self.failed_mapping(subject, "rebuild_tree", &error, explained);
                return;
            }
        };
        let in_order = tree.nodes.len() == candidates.len()
            && tree
                .nodes
                .iter()
                .zip(candidates)
                .all(|(node, candidate)| node.candidate == candidate.location.offset);
        self.check(Rule::Consistency, case, in_order, || {
            format!(
                "the tree holds {} node(s) for {} candidate(s), not one per candidate in input order",
                tree.nodes.len(),
                candidates.len()
            )
        });
        let known: HashSet<u64> = candidates
            .iter()
            .map(|candidate| candidate.location.offset)
            .collect();
        let unresolved: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| {
                node.parents.iter().enumerate().any(|(index, parent)| {
                    node.parents[..index].contains(parent)
                        || matches!(parent, Parent::Candidate(offset) if !known.contains(offset))
                })
            })
            .collect();
        self.check(Rule::Consistency, case, unresolved.is_empty(), || {
            format!("nodes name a parent twice or a parent that is not a candidate: {unresolved:?}")
        });
        let again = adapter.rebuild_tree(*image, candidates);
        self.check(
            Rule::Consistency,
            case,
            again.as_ref().ok() == Some(&tree),
            || "a second tree differs from the first".into(),
        );
        let reversed: Vec<_> = candidates.iter().rev().cloned().collect();
        let flipped = adapter.rebuild_tree(*image, &reversed);
        let same = flipped.as_ref().is_ok_and(|flipped| {
            flipped.nodes.len() == tree.nodes.len()
                && flipped
                    .nodes
                    .iter()
                    .rev()
                    .zip(&tree.nodes)
                    .all(|(one, other)| {
                        one.candidate == other.candidate
                            && one.parents.len() == other.parents.len()
                            && one
                                .parents
                                .iter()
                                .all(|parent| other.parents.contains(parent))
                    })
        });
        self.check(Rule::Consistency, case, same, || {
            "rebuilding from the candidates in reverse order changes their parent claims".into()
        });
        let empty = adapter.rebuild_tree(*image, &[]);
        self.check(
            Rule::Consistency,
            case,
            matches!(&empty, Ok(tree) if tree.nodes.is_empty()),
            || format!("a tree of no candidates is not empty: {empty:?}"),
        );
        if !tree.nodes.is_empty() {
            self.exercised.insert(Feature::RebuildTree);
        }
    }

    /// Reads an extent through the adapter and compares the source's bytes.
    fn read_back(&mut self, subject: &Subject<'_, D::Adapter>, extent: Extent) {
        let case = subject.case;
        let length = extent.length.min(READ_BYTES);
        let mut output = vec![0; length as usize];
        let result = subject
            .adapter
            .read_extent(subject.image, extent, 0, &mut output);
        if !subject.capabilities.supports(Feature::ReadExtent) {
            self.refused(case, Feature::ReadExtent, &result);
            return;
        }
        let span = Extent::new(extent.offset, length);
        match result {
            Ok(count) => {
                let same = count == output.len() && subject.image.slice(span) == Some(&output[..]);
                self.check(Rule::Consistency, case, same, || {
                    format!("reading {span:?} by extent returned {count} byte(s) other than the source holds")
                });
                if count > 0 {
                    self.exercised.insert(Feature::ReadExtent);
                }
            }
            Err(SalvageError::Device(_)) if subject.image.overlaps_unreadable(span) => {
                self.check(Rule::Reported, case, true, String::new);
            }
            Err(error) => self.failed(subject, "read_extent", &error),
        }
    }

    /// Reads a probe extent near the start of the source, part way into it,
    /// at its end and past it.
    fn reads(&mut self, subject: &Subject<'_, D::Adapter>) {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let size = image.size();
        let probe = Extent::new(1, size.saturating_sub(1).min(PROBE_BYTES));
        self.read_back(subject, probe);
        if !capabilities.supports(Feature::ReadExtent) {
            return;
        }
        let into = probe.length / 2;
        let mut output = [0u8; 16];
        let result = adapter.read_extent(*image, probe, into, &mut output);
        let count = (probe.length - into).min(output.len() as u64);
        let span = Extent::new(probe.offset + into, count);
        match result {
            Ok(read) => {
                let same =
                    read as u64 == count && image.slice(span) == Some(&output[..count as usize]);
                self.check(Rule::Consistency, case, same, || {
                    format!("reading {probe:?} from {into} bytes in, into a short buffer, did not return {span:?}")
                });
            }
            Err(SalvageError::Device(_)) if image.overlaps_unreadable(span) => {
                self.check(Rule::Reported, case, true, String::new);
            }
            Err(error) => self.failed(subject, "read_extent", &error),
        }
        let end = adapter.read_extent(*image, probe, probe.length, &mut output);
        self.check(Rule::Consistency, case, matches!(end, Ok(0)), || {
            format!("reading at the end of {probe:?} did not return zero bytes: {end:?}")
        });
        for (extent, offset) in [
            (probe, probe.length + 1),
            (Extent::new(size.saturating_sub(1), 2), 0),
            (Extent::new(u64::MAX, 2), 0),
        ] {
            let result = adapter.read_extent(*image, extent, offset, &mut output);
            self.check(
                Rule::Consistency,
                case,
                matches!(result, Err(SalvageError::Bounds)),
                || format!("reading {extent:?} from {offset} bytes in was not refused as out of bounds: {result:?}"),
            );
        }
    }

    fn materialize(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        walks: &[(CandidateState, ExtentWalk)],
    ) {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let size = image.size();
        let mut steps = vec![PlanStep::Bytes(b"plan".to_vec()), PlanStep::Zeros(0)];
        let mut budget = PLAN_BYTES;
        for extent in walks
            .iter()
            .flat_map(|(_, walk)| &walk.extents)
            .take(PLAN_EXTENTS)
        {
            let length = extent.length.min(budget);
            if length == 0 {
                break;
            }
            steps.push(PlanStep::Extent(Extent::new(extent.offset, length)));
            budget -= length;
        }
        steps.push(PlanStep::Zeros(PROBE_BYTES + 1));
        steps.push(PlanStep::Extent(Extent::new(
            1,
            size.saturating_sub(1).min(PROBE_BYTES),
        )));
        steps.push(PlanStep::Bytes(Vec::new()));
        let plan = Plan::new(steps);
        let mut output = Vec::new();
        let result = adapter.materialize(*image, &plan, &mut output);
        if !capabilities.supports(Feature::Materialize) {
            self.refused(case, Feature::Materialize, &result);
            return;
        }
        match result {
            Ok(report) => {
                self.provenance(subject, &plan, &report, &output);
                self.positioned(subject, &plan, &report, &output);
            }
            Err(error) => self.failed(subject, "materialize", &error),
        }

        let past = Plan::new(vec![
            PlanStep::Bytes(b"x".to_vec()),
            PlanStep::Extent(Extent::new(size, 1)),
        ]);
        let mut output = Vec::new();
        let result = adapter.materialize(*image, &past, &mut output);
        self.check(
            Rule::Consistency,
            case,
            matches!(result, Err(SalvageError::Bounds)) && output.is_empty(),
            || format!("a plan reaching past the source was not refused before anything was written: {result:?}"),
        );
        let small = Plan::new(vec![PlanStep::Bytes(b"x".to_vec())]);
        let result = adapter.materialize(*image, &small, &mut Refusing);
        self.check(
            Rule::Consistency,
            case,
            matches!(result, Err(SalvageError::Sink(_))),
            || format!("a failing sink was not reported as a sink failure: {result:?}"),
        );
    }

    /// Cluster steps must materialize exactly the bytes and provenance of
    /// the extents they map to, cut by a final length and zeroed past a valid
    /// length, and a cluster outside the layout must be refused before
    /// anything is written.
    fn clusters(&mut self, subject: &Subject<'_, D::Adapter>, explained: bool) {
        let Subject {
            case,
            image,
            adapter,
            capabilities,
            ..
        } = subject;
        let layout = adapter.cluster_layout();
        if !capabilities.supports(Feature::ClusterSteps) {
            self.refused(case, Feature::ClusterSteps, &layout);
            if capabilities.supports(Feature::Materialize) {
                let plan = Plan::new(vec![PlanStep::Clusters { first: 0, count: 1 }]);
                let result = adapter.materialize(*image, &plan, &mut Vec::new());
                self.refused(case, Feature::ClusterSteps, &result);
            }
            return;
        }
        let layout = match layout {
            Ok(layout) => layout,
            Err(error) => {
                self.failed_mapping(subject, "cluster_layout", &error, explained);
                return;
            }
        };
        if !capabilities.supports(Feature::Materialize) {
            return;
        }
        let outside = Plan::new(vec![
            PlanStep::Bytes(b"x".to_vec()),
            PlanStep::Clusters {
                first: layout.first.saturating_add(layout.count),
                count: 1,
            },
        ]);
        let mut output = Vec::new();
        let result = adapter.materialize(*image, &outside, &mut output);
        self.check(
            Rule::Consistency,
            case,
            matches!(
                result,
                Err(SalvageError::Plan(PlanFault::ClustersOutOfRange { step: 1 }))
            ) && output.is_empty(),
            || format!("a plan naming a cluster past the layout was not refused before anything was written: {result:?}"),
        );

        let count = layout.count.min(2);
        let Some(extent) = layout.extent(layout.first, count).filter(|extent| {
            extent.length > 0 && extent.end().is_some_and(|end| end <= image.size())
        }) else {
            return;
        };
        let length = extent.length - u64::from(extent.length > 1);
        let valid = length / 2;
        let mut plan = Plan::new(vec![PlanStep::Clusters {
            first: layout.first,
            count,
        }]);
        plan.length = Some(length);
        plan.valid_length = Some(valid);
        let plain = Plan::new(vec![
            PlanStep::Extent(Extent::new(extent.offset, valid)),
            PlanStep::Zeros(length - valid),
        ]);
        let mut output = Vec::new();
        let result = adapter.materialize(*image, &plan, &mut output);
        let mut expected = Vec::new();
        let reference = adapter.materialize(*image, &plain, &mut expected);
        match (result, reference) {
            (Ok(report), Ok(reference)) => {
                self.provenance(subject, &plain, &reference, &expected);
                let same = report == reference && output == expected;
                self.check(Rule::Consistency, case, same, || {
                    format!("clusters {}..{} with length {length} and valid length {valid} materialized differently from {extent:?}: {report:?}", layout.first, layout.first + count)
                });
                if report
                    .ranges
                    .iter()
                    .any(|range| range.readable && range.source.is_some())
                {
                    self.exercised.insert(Feature::ClusterSteps);
                }
            }
            (Err(error), _) | (_, Err(error)) => self.failed(subject, "materialize", &error),
        }
    }

    /// Reading the plan's output in odd-sized pieces from its reader must give
    /// the bytes and provenance that materializing it gave.
    fn positioned(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        plan: &Plan,
        report: &MaterializeReport,
        output: &[u8],
    ) {
        let case = subject.case;
        let reader = match subject.adapter.plan_reader(subject.image, plan) {
            Ok(reader) => reader,
            Err(error) => return self.failed(subject, "plan_reader", &error),
        };
        let mut bytes = Vec::with_capacity(output.len());
        let mut ranges: Vec<MaterializedRange> = Vec::new();
        let mut buffer = vec![0; READ_PIECE];
        let mut offset = 0;
        let mut answered = true;
        while offset < reader.len() {
            let read = match reader.read_at(offset, &mut buffer) {
                Ok(read) => read,
                Err(error) => {
                    answered = false;
                    self.failed(subject, "read_at", &error);
                    break;
                }
            };
            bytes.extend_from_slice(&buffer[..read.length as usize]);
            for range in read.ranges {
                match ranges.last_mut() {
                    Some(last)
                        if last.readable == range.readable
                            && last.output.end() == Some(range.output.offset)
                            && match (last.source, range.source) {
                                (Some(previous), Some(next)) => previous.end() == Some(next.offset),
                                (None, None) => true,
                                _ => false,
                            } =>
                    {
                        last.output.length += range.output.length;
                        if let (Some(previous), Some(next)) = (last.source.as_mut(), range.source) {
                            previous.length += next.length;
                        }
                    }
                    _ => ranges.push(range),
                }
            }
            if read.length == 0 {
                break;
            }
            offset += read.length;
        }
        if !answered {
            return;
        }
        self.check(
            Rule::Consistency,
            case,
            reader.len() == report.length && bytes == output && ranges == report.ranges,
            || {
                "reading the plan's output from its reader in pieces differs from materializing it"
                    .into()
            },
        );
        let past = reader.read_at(reader.len() + 1, &mut buffer);
        self.check(
            Rule::Consistency,
            case,
            matches!(past, Err(SalvageError::Bounds)),
            || format!("a read past the plan's output was not refused as out of bounds: {past:?}"),
        );
    }

    /// Checks a materialized output against its plan and its provenance
    /// against the source. The plan has no cluster steps or declared lengths.
    fn provenance(
        &mut self,
        subject: &Subject<'_, D::Adapter>,
        plan: &Plan,
        report: &MaterializeReport,
        output: &[u8],
    ) {
        let case = subject.case;
        let image = subject.image;
        let written = output.len() as u64;
        self.check(Rule::ReadOnly, case, written == report.length, || {
            format!(
                "the sink received {written} byte(s), and the report names {}",
                report.length
            )
        });
        self.check(
            Rule::Consistency,
            case,
            plan.length() == Some(report.length),
            || {
                format!(
                    "the plan is {:?} bytes long, and the report names {}",
                    plan.length(),
                    report.length
                )
            },
        );
        let mut position = 0;
        let mut contiguous = true;
        for range in &report.ranges {
            contiguous &= range.output.offset == position && range.output.length > 0;
            position = range.output.end().unwrap_or(u64::MAX);
        }
        contiguous &= position == report.length;
        self.check(Rule::Consistency, case, contiguous, || {
            format!(
                "materialized ranges do not cover the output once, in order: {:?}",
                report.ranges
            )
        });
        if !contiguous || written != report.length {
            return;
        }

        let mut expected = Vec::with_capacity(output.len());
        for step in &plan.steps {
            match step {
                PlanStep::Bytes(bytes) => expected.extend_from_slice(bytes),
                PlanStep::Zeros(length) => expected.resize(expected.len() + *length as usize, 0),
                PlanStep::Extent(extent) => {
                    expected.extend_from_slice(image.slice(*extent).unwrap_or_default())
                }
                PlanStep::Clusters { .. } => {}
            }
        }
        let mut sources = Vec::new();
        for range in &report.ranges {
            let output_range =
                range.output.offset as usize..(range.output.offset + range.output.length) as usize;
            let Some(source) = range.source else {
                self.check(Rule::Consistency, case, range.readable, || {
                    format!(
                        "plan zeros or bytes at {:?} are flagged unreadable",
                        range.output
                    )
                });
                continue;
            };
            sources.push(source);
            let honest = source.length == range.output.length
                && range.readable != image.overlaps_unreadable(source);
            self.check(Rule::Reported, case, honest, || {
                format!("output {:?} from source {source:?} is flagged readable: {}, which the source bytes contradict", range.output, range.readable)
            });
            if !range.readable && output_range.end <= expected.len() {
                expected[output_range].fill(0);
            }
            let named = report.source_of(range.output.offset) == Some(source.offset);
            self.check(Rule::Consistency, case, named, || {
                format!(
                    "source_of does not name {source:?} for output {:?}",
                    range.output
                )
            });
        }
        self.check(Rule::Consistency, case, expected == output, || {
            "materialized output differs from the plan's bytes".into()
        });
        let planned: Vec<_> = plan
            .steps
            .iter()
            .filter_map(|step| match step {
                PlanStep::Extent(extent) => Some(*extent),
                _ => None,
            })
            .collect();
        self.check(
            Rule::Consistency,
            case,
            joined(&planned) == joined(&sources),
            || "materialized provenance does not name the plan's source extents in order".into(),
        );
        if report
            .ranges
            .iter()
            .any(|range| range.readable && range.source.is_some())
        {
            self.exercised.insert(Feature::Materialize);
        }
    }

    fn healthy(&mut self, image: &Image, observed: &Observed<Detail<D>>) {
        let case = "healthy";
        let strict = self.driver.strict(image);
        self.check(Rule::StrictRejects, case, strict.is_ok(), || {
            format!("the strict API rejects the healthy image: {strict:?}")
        });
        if let Some(findings) = &observed.findings {
            self.check(Rule::Reported, case, findings.is_empty(), || {
                format!("the healthy image reports damage: {findings:?}")
            });
        }
        let failed: Vec<_> = observed
            .candidates
            .iter()
            .filter(|candidate| candidate.integrity == Integrity::Failed)
            .map(|candidate| candidate.location)
            .collect();
        self.check(Rule::Reported, case, failed.is_empty(), || {
            format!("candidates on the healthy image fail their integrity checks: {failed:?}")
        });
        let broken: Vec<_> = observed
            .walks
            .iter()
            .filter(|(state, walk)| {
                *state != CandidateState::Deleted && walk.end != WalkEnd::Complete
            })
            .collect();
        self.check(Rule::Reported, case, broken.is_empty(), || {
            format!("live candidates on the healthy image have broken walks: {broken:?}")
        });
    }

    fn damaged(
        &mut self,
        case: &str,
        image: &Image,
        evidence: Evidence,
        observed: &Observed<Detail<D>>,
    ) {
        let strict = self.driver.strict(image);
        self.check(Rule::StrictRejects, case, strict.is_err(), || {
            "the strict API accepts the damaged image".into()
        });
        let shown = match evidence {
            Evidence::Damage(kind) => observed
                .findings
                .as_ref()
                .is_some_and(|findings| findings.iter().any(|finding| finding.kind == kind)),
            Evidence::FailedCandidate => observed
                .candidates
                .iter()
                .any(|candidate| candidate.integrity == Integrity::Failed),
            Evidence::BrokenWalk => observed.walks.iter().any(|(state, walk)| {
                *state != CandidateState::Deleted && walk.end != WalkEnd::Complete
            }),
        };
        self.check(Rule::Reported, case, shown, || {
            format!("the salvage path does not show the declared evidence {evidence:?}")
        });
    }

    /// Runs the set on two sources derived from the healthy image: one whose
    /// every read fails, and one whose first candidate's record cannot be read.
    fn unreadable(&mut self, healthy: &Image, observed: &Observed<Detail<D>>) {
        let size = healthy.size();
        let whole = Extent::new(0, size);
        let dead = Image::new(healthy.bytes.clone(), &[whole]);
        let case = "every read fails";

        // An adapter keeps format context only, so one built from the healthy
        // image must decode nothing from a source it cannot read.
        let adapter = self.driver.salvage(healthy);
        let capabilities = adapter.capabilities();
        if capabilities.supports(Feature::FindCandidates) {
            let mut found = Vec::new();
            let result = adapter.find_candidates(&dead, whole, &mut found);
            let blind =
                matches!(&result, Ok(report) if !report.unreadable.is_empty()) && found.is_empty();
            self.check(Rule::ReadOnly, case, blind, || {
                format!("an adapter built from the healthy image decoded {} candidate(s) from a source it cannot read, so it holds a source of its own: {result:?}", found.len())
            });
        }
        if capabilities.supports(Feature::ReadExtent) && size > 0 {
            let mut output = vec![0; size.min(SECTOR) as usize];
            let span = Extent::new(0, output.len() as u64);
            let result = adapter.read_extent(&dead, span, 0, &mut output);
            self.check(
                Rule::ReadOnly,
                case,
                matches!(result, Err(SalvageError::Device(_))),
                || format!("an adapter built from the healthy image read {span:?} from a source it cannot read: {result:?}"),
            );
        }

        let seen = self.examine(case, Input::Damaged, &dead);
        let strict = self.driver.strict(&dead);
        self.check(Rule::StrictRejects, case, strict.is_err(), || {
            "the strict API accepts a source it cannot read".into()
        });
        if let Some(findings) = &seen.findings {
            let reported = findings
                .iter()
                .any(|finding| finding.kind == DamageKind::Unreadable);
            self.check(Rule::Reported, case, reported, || {
                format!(
                    "a source whose every read fails reports no unreadable structure: {findings:?}"
                )
            });
        }

        let Some(first) = observed.candidates.first() else {
            return;
        };
        let case = "first record unreadable";
        let image = Image::new(healthy.bytes.clone(), &[first.location]);
        let adapter = self.driver.salvage(&image);
        let mut found = Vec::new();
        if let Ok(report) = adapter.find_candidates(&image, whole, &mut found) {
            self.check(
                Rule::Reported,
                case,
                covered(&report.unreadable, first.location),
                || format!("the record at {:?} was decoded before and cannot be read now, yet the scan does not list it: {:?}", first.location, report.unreadable),
            );
        }
        let seen = self.examine(case, Input::Damaged, &image);
        let still = seen
            .candidates
            .iter()
            .any(|candidate| candidate.location.offset == first.location.offset);
        self.check(Rule::Reported, case, !still, || {
            format!(
                "the candidate at {:?} is still reported though its record cannot be read",
                first.location
            )
        });
    }

    /// Every supported feature must have been exercised, and every missing
    /// one probed for its refusal.
    fn exercised(&mut self, capabilities: &Capabilities) {
        for feature in FEATURES {
            if capabilities.supports(feature) {
                let used = self.exercised.contains(&feature);
                self.check(Rule::Capabilities, "run", used, || {
                    format!("{feature} is supported, yet nothing in the run exercised it; an unexercised capability is not a passing one")
                });
            } else {
                let probed = self.probed.contains(&feature);
                self.check(Rule::Capabilities, "run", probed, || {
                    format!("{feature} is listed as missing, yet the run never saw it refused")
                });
            }
        }
    }
}

fn plain_data<T: Serialize + DeserializeOwned>() {}

/// Up to [`SAMPLE`] indices spread over `0..len`, the first and last included.
fn sample(len: usize) -> Vec<usize> {
    if len <= SAMPLE {
        return (0..len).collect();
    }
    let mut picked: Vec<_> = (0..SAMPLE)
        .map(|index| index * (len - 1) / (SAMPLE - 1))
        .collect();
    picked.dedup();
    picked
}

fn overlaps(one: Extent, other: Extent) -> bool {
    let one_end = one.end().unwrap_or(u64::MAX);
    let other_end = other.end().unwrap_or(u64::MAX);
    one.length > 0 && other.length > 0 && one.offset < other_end && other.offset < one_end
}

/// Whether `listed` covers every byte of `target`.
fn covered(listed: &[Extent], target: Extent) -> bool {
    let mut sorted = listed.to_vec();
    sorted.sort_unstable();
    let end = target.end().unwrap_or(u64::MAX);
    let mut position = target.offset;
    for extent in sorted {
        if position >= end {
            break;
        }
        if extent.offset <= position {
            position = position.max(extent.end().unwrap_or(u64::MAX));
        }
    }
    position >= end
}

/// Extents with empty ones dropped and adjacent ones joined.
fn joined(extents: &[Extent]) -> Vec<Extent> {
    let mut joined: Vec<Extent> = Vec::new();
    for extent in extents.iter().filter(|extent| extent.length > 0) {
        match joined.last_mut() {
            Some(last) if last.end() == Some(extent.offset) => last.length += extent.length,
            _ => joined.push(*extent),
        }
    }
    joined
}

/// The source every check reads: an in-memory image with no write path,
/// whose reads fail wherever they touch an unreadable sector.
struct Image {
    bytes: Vec<u8>,
    unreadable: Vec<Extent>,
}

impl Image {
    fn new(bytes: Vec<u8>, unreadable: &[Extent]) -> Self {
        let size = bytes.len() as u64;
        let unreadable = unreadable
            .iter()
            .filter(|extent| extent.length > 0 && extent.offset < size)
            .map(|extent| {
                let start = extent.offset / SECTOR * SECTOR;
                let end = extent
                    .end()
                    .and_then(|end| end.checked_next_multiple_of(SECTOR))
                    .map_or(size, |end| end.min(size));
                Extent::new(start, end - start)
            })
            .collect();
        Self { bytes, unreadable }
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn overlaps_unreadable(&self, extent: Extent) -> bool {
        self.unreadable.iter().any(|bad| overlaps(*bad, extent))
    }

    fn slice(&self, extent: Extent) -> Option<&[u8]> {
        let start = usize::try_from(extent.offset).ok()?;
        let end = usize::try_from(extent.end()?).ok()?;
        self.bytes.get(start..end)
    }
}

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let extent = Extent::new(offset, output.len() as u64);
        let Some(bytes) = self.slice(extent) else {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: extent.length,
                size: self.size(),
            });
        };
        if self.overlaps_unreadable(extent) {
            return Err(io::Error::other("unreadable sector").into());
        }
        output.copy_from_slice(bytes);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size()
    }
}

/// A sink that refuses every write.
struct Refusing;

impl io::Write for Refusing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sink refused the write"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! The set run against a toy format, as conforming drivers and as drivers
    //! that each break one rule. Header: magic, record count, checksum.
    //! Records: 64-byte slots after it, each naming a data extent.

    use super::*;
    use crate::salvage::{CandidateKind, TreeNode};
    use serde::Deserialize;

    const SLOT: u64 = 64;
    const SIZE: usize = 16 * 1024;

    /// The one way a toy driver departs from the convention.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Quirk {
        None,
        /// A format with no deleted state lists the deletion parts as missing.
        ReadOnlyFormat,
        /// Counts one candidate more than it streams.
        MiscountedScan,
        /// Fails its damage report when it finds damage.
        RejectsDamage,
        /// Its strict API accepts anything.
        LaxStrict,
        /// Lists the tree as missing, yet builds one.
        HidesTree,
        /// Copies the source it is built from and reads that copy.
        KeepsSource,
        /// Supports deleted candidates, but no image holds one.
        NoDeletedRecord,
    }

    struct Toy(Quirk);

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Start(u64);

    struct Adapter {
        quirk: Quirk,
        kept: Option<Vec<u8>>,
    }

    struct Record {
        deleted: bool,
        start: u64,
        length: u64,
        name: String,
        checksum_ok: bool,
    }

    fn sum(bytes: &[u8]) -> u32 {
        bytes.iter().fold(0u32, |sum, byte| {
            sum.wrapping_mul(31).wrapping_add(u32::from(*byte))
        })
    }

    fn le(bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .rev()
            .fold(0, |value, byte| value << 8 | u64::from(*byte))
    }

    /// Deletion changes the state byte and keeps the live checksum.
    fn record(state: u8, start: u64, length: u64, name: &str) -> Vec<u8> {
        let mut slot = vec![0; SLOT as usize];
        slot[..4].copy_from_slice(b"RECL");
        slot[4..12].copy_from_slice(&start.to_le_bytes());
        slot[12..20].copy_from_slice(&length.to_le_bytes());
        slot[20..20 + name.len()].copy_from_slice(name.as_bytes());
        let checksum = sum(&slot[..60]);
        slot[60..].copy_from_slice(&checksum.to_le_bytes());
        slot[3] = state;
        slot
    }

    fn decode(slot: &[u8]) -> Option<Record> {
        if &slot[..3] != b"REC" || !matches!(slot[3], b'L' | b'D') {
            return None;
        }
        let mut live = slot.to_vec();
        live[3] = b'L';
        let name = slot[20..36].iter().take_while(|byte| **byte != 0).copied();
        Some(Record {
            deleted: slot[3] == b'D',
            start: le(&slot[4..12]),
            length: le(&slot[12..20]),
            name: String::from_utf8_lossy(&name.collect::<Vec<_>>()).into_owned(),
            checksum_ok: u64::from(sum(&live[..60])) == le(&slot[60..64]),
        })
    }

    fn healthy(quirk: Quirk) -> Vec<u8> {
        let mut records = vec![
            record(b'L', 1024, 100, "alpha"),
            record(b'L', 2048, 5000, "beta"),
        ];
        if quirk != Quirk::NoDeletedRecord {
            records.push(record(b'D', 8192, 10, "gamma"));
        }
        let mut image = vec![0; SIZE];
        image[..4].copy_from_slice(b"TOY!");
        image[4..8].copy_from_slice(&(records.len() as u32).to_le_bytes());
        let checksum = sum(&image[..8]);
        image[8..12].copy_from_slice(&checksum.to_le_bytes());
        for (index, slot) in records.iter().enumerate() {
            let at = (index + 1) * SLOT as usize;
            image[at..at + slot.len()].copy_from_slice(slot);
        }
        for (index, byte) in image[1024..8202].iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        image
    }

    impl Driver for Toy {
        type Adapter = Adapter;
        type StrictError = String;

        fn salvage(&self, source: &dyn BlockRead) -> Adapter {
            let mut kept = None;
            if self.0 == Quirk::KeepsSource {
                let mut bytes = vec![0; source.size_bytes() as usize];
                kept = source.read_at(0, &mut bytes).ok().map(|()| bytes);
            }
            Adapter {
                quirk: self.0,
                kept,
            }
        }

        fn strict(&self, source: &dyn BlockRead) -> Result<(), String> {
            if self.0 == Quirk::LaxStrict {
                return Ok(());
            }
            let mut header = [0; 12];
            source
                .read_at(0, &mut header)
                .map_err(|error| error.to_string())?;
            if &header[..4] != b"TOY!" || u64::from(sum(&header[..8])) != le(&header[8..]) {
                return Err("header".into());
            }
            for index in 1..=le(&header[4..8]) {
                let mut slot = [0; SLOT as usize];
                source
                    .read_at(index * SLOT, &mut slot)
                    .map_err(|error| error.to_string())?;
                let record = decode(&slot).ok_or("record")?;
                if !record.checksum_ok {
                    return Err("record checksum".into());
                }
                if !record.deleted {
                    let mut data = vec![0; record.length as usize];
                    source
                        .read_at(record.start, &mut data)
                        .map_err(|error| error.to_string())?;
                }
            }
            Ok(())
        }

        fn healthy(&self) -> Vec<u8> {
            healthy(self.0)
        }

        fn damaged(&self) -> Vec<Damaged> {
            let damaged = |name: &str, evidence, at: usize| {
                let mut image = healthy(self.0);
                image[at] ^= 1;
                Damaged::new(name, image, evidence)
            };
            let mut past = healthy(self.0);
            past[2 * SLOT as usize..3 * SLOT as usize].copy_from_slice(&record(
                b'L',
                2048,
                SIZE as u64,
                "beta",
            ));
            vec![
                damaged("header checksum", Evidence::Damage(DamageKind::Checksum), 9),
                damaged("record checksum", Evidence::FailedCandidate, 64 + 20),
                Damaged::new("data past the end", past, Evidence::BrokenWalk),
                Damaged::new(
                    "unreadable header",
                    healthy(self.0),
                    Evidence::Damage(DamageKind::Unreadable),
                )
                .unreadable(Extent::new(0, 12)),
            ]
        }
    }

    impl Adapter {
        fn read(
            &self,
            source: &dyn BlockRead,
            offset: u64,
            output: &mut [u8],
        ) -> fs_core::Result<()> {
            match &self.kept {
                Some(bytes) => {
                    output.copy_from_slice(&bytes[offset as usize..offset as usize + output.len()]);
                    Ok(())
                }
                None => source.read_at(offset, output),
            }
        }
    }

    impl SalvageAdapter for Adapter {
        type Detail = Start;

        fn capabilities(&self) -> Capabilities {
            let missing = match self.quirk {
                Quirk::ReadOnlyFormat => vec![
                    Feature::DeletedCandidates,
                    Feature::DeletedExtents,
                    Feature::ClusterSteps,
                ],
                Quirk::HidesTree => vec![Feature::RebuildTree, Feature::ClusterSteps],
                _ => vec![Feature::ClusterSteps],
            };
            Capabilities { missing }
        }

        fn find_candidates(
            &self,
            source: &dyn BlockRead,
            range: Extent,
            sink: &mut dyn Sink<Candidate<Start>>,
        ) -> Result<ScanReport, SalvageError> {
            let size = source.size_bytes();
            let end = range
                .end()
                .filter(|end| *end <= size)
                .ok_or(SalvageError::Bounds)?;
            let mut report = ScanReport::default();
            let mut position = range.offset.next_multiple_of(SLOT);
            while position < end && position + SLOT <= size {
                let mut slot = [0; SLOT as usize];
                if self.read(source, position, &mut slot).is_err() {
                    let length = SLOT.min(end - position);
                    match report.unreadable.last_mut() {
                        Some(last) if last.end() == Some(position) => last.length += length,
                        _ => report.unreadable.push(Extent::new(position, length)),
                    }
                } else if let Some(record) = decode(&slot) {
                    if !(record.deleted && self.quirk == Quirk::ReadOnlyFormat) {
                        report.candidates += 1;
                        let candidate = Candidate {
                            location: Extent::new(position, SLOT),
                            kind: CandidateKind::File,
                            state: if record.deleted {
                                CandidateState::Deleted
                            } else {
                                CandidateState::Live
                            },
                            integrity: if record.checksum_ok {
                                Integrity::Verified
                            } else {
                                Integrity::Failed
                            },
                            name: Some(record.name),
                            size: Some(record.length),
                            detail: Start(record.start),
                        };
                        if sink.accept(candidate).is_break() {
                            report.stopped_at = Some(position + SLOT);
                            return Ok(report);
                        }
                    }
                }
                position += SLOT;
            }
            if self.quirk == Quirk::MiscountedScan {
                report.candidates += 1;
            }
            Ok(report)
        }

        fn walk_extents(
            &self,
            source: &dyn BlockRead,
            candidate: &Candidate<Start>,
        ) -> Result<ExtentWalk, SalvageError> {
            if candidate.state == CandidateState::Deleted {
                self.capabilities().require(Feature::DeletedExtents)?;
            }
            let size = source.size_bytes();
            let claimed = candidate.size.unwrap_or(0);
            let start = candidate.detail.0;
            let (extents, end) = match start.checked_add(claimed) {
                Some(stop) if stop <= size => (
                    (claimed > 0)
                        .then(|| Extent::new(start, claimed))
                        .into_iter()
                        .collect(),
                    WalkEnd::Complete,
                ),
                _ if start < size => (vec![Extent::new(start, size - start)], WalkEnd::OutOfRange),
                _ => (Vec::new(), WalkEnd::OutOfRange),
            };
            Ok(ExtentWalk {
                extents,
                claimed,
                end,
            })
        }

        fn rebuild_tree(
            &self,
            _source: &dyn BlockRead,
            candidates: &[Candidate<Start>],
        ) -> Result<Tree, SalvageError> {
            let nodes = candidates
                .iter()
                .map(|candidate| TreeNode {
                    candidate: candidate.location.offset,
                    parents: vec![Parent::Root],
                })
                .collect();
            Ok(Tree { nodes })
        }

        fn report_damage(
            &self,
            source: &dyn BlockRead,
            sink: &mut dyn Sink<Damage>,
        ) -> Result<DamageReport, SalvageError> {
            let mut header = [0; 12];
            let found = match self.read(source, 0, &mut header) {
                Err(error) => Some((DamageKind::Unreadable, error.to_string())),
                Ok(()) if &header[..4] != b"TOY!" => Some((DamageKind::Signature, "magic".into())),
                Ok(()) if u64::from(sum(&header[..8])) != le(&header[8..]) => {
                    Some((DamageKind::Checksum, "header".into()))
                }
                Ok(()) => None,
            };
            let mut report = DamageReport::default();
            if let Some((kind, detail)) = found {
                if self.quirk == Quirk::RejectsDamage {
                    return Err(SalvageError::NoGeometry);
                }
                report.reported = 1;
                report.stopped = sink
                    .accept(Damage {
                        kind,
                        severity: Severity::Error,
                        location: Extent::new(0, 12),
                        structure: "header".into(),
                        detail,
                    })
                    .is_break();
            }
            Ok(report)
        }

        fn read_extent(
            &self,
            source: &dyn BlockRead,
            extent: Extent,
            offset: u64,
            output: &mut [u8],
        ) -> Result<usize, SalvageError> {
            if offset > extent.length || extent.end().is_none_or(|end| end > source.size_bytes()) {
                return Err(SalvageError::Bounds);
            }
            let count = (extent.length - offset).min(output.len() as u64) as usize;
            self.read(source, extent.offset + offset, &mut output[..count])
                .map_err(SalvageError::Device)?;
            Ok(count)
        }
    }

    #[test]
    fn conforming_drivers_pass_with_and_without_deletion_parts() {
        for quirk in [Quirk::None, Quirk::ReadOnlyFormat] {
            let report = run(&Toy(quirk));
            assert!(report.conforms(), "{quirk:?}: {report}");
        }
    }

    #[test]
    fn each_broken_rule_is_named() {
        // A report that always fails also never exercises the capability.
        let rejected = [Rule::Reported, Rule::Capabilities];
        for (quirk, broken) in [
            (Quirk::MiscountedScan, &[Rule::Consistency][..]),
            (Quirk::KeepsSource, &[Rule::ReadOnly]),
            (Quirk::RejectsDamage, &rejected),
            (Quirk::LaxStrict, &[Rule::StrictRejects]),
            (Quirk::HidesTree, &[Rule::Capabilities]),
            (Quirk::NoDeletedRecord, &[Rule::Capabilities]),
        ] {
            let report = run(&Toy(quirk));
            assert!(!report.conforms());
            let rules: HashSet<_> = report
                .violations()
                .iter()
                .map(|violation| violation.rule)
                .collect();
            assert_eq!(
                rules,
                HashSet::from_iter(broken.iter().copied()),
                "{quirk:?}: {report}"
            );
        }
    }

    #[test]
    fn a_rule_that_ran_nothing_fails_the_report() {
        let mut report = run(&Toy(Quirk::None));
        assert!(report.conforms());
        report.checks[Rule::StrictRejects as usize] = 0;
        assert!(!report.conforms());
        assert!(report
            .to_string()
            .contains("strict API rejects damage: 0 check(s), so it proves nothing"));
    }
}
