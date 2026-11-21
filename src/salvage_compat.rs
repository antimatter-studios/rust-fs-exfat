use super::*;
use fs_core::layers::salvage as shared;

macro_rules! convert_enum {
    ($from:path => $to:path; $($variant:ident $(($value:ident $(, $convert:expr)?))? $({$($field:ident),+})?),+ $(,)?) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                use $from as Source;
                match value {
                    $(Source::$variant $(($value))? $({$($field),+})? =>
                        Self::$variant $((convert_enum!(@field $value $(, $convert)?)))? $({$($field),+})?,)+
                }
            }
        }
    };
    (@field $value:ident) => { $value };
    (@field $value:ident, $convert:expr) => { ($convert)($value) };
}

macro_rules! convert_both_enums {
    ($name:ident; $($variants:tt)*) => {
        convert_enum!($name => shared::$name; $($variants)*);
        convert_enum!(shared::$name => $name; $($variants)*);
    };
}

macro_rules! convert_struct {
    ($from:path => $to:path; $($field:ident $(: $convert:expr)?),+ $(,)?) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                Self { $($field: convert_struct!(@field value.$field $(, $convert)?)),+ }
            }
        }
    };
    (@field $value:expr) => { $value };
    (@field $value:expr, $convert:expr) => { ($convert)($value) };
}

macro_rules! convert_both_structs {
    ($name:ident; $($fields:tt)*) => {
        convert_struct!($name => shared::$name; $($fields)*);
        convert_struct!(shared::$name => $name; $($fields)*);
    };
}

fn convert_vec<T: Into<U>, U>(items: Vec<T>) -> Vec<U> {
    items.into_iter().map(Into::into).collect()
}

convert_both_enums!(CandidateKind; File, Directory, Structure, Other);
convert_both_enums!(CandidateState; Live, Deleted, Unknown);
convert_both_enums!(Integrity; Verified, Failed, Unchecked);
convert_both_enums!(WalkEnd; Complete, Broken, OutOfRange, Cycle, Unreadable(extent, Into::into));
convert_both_enums!(Parent; Root, Candidate(offset));
convert_both_enums!(Severity; Notice, Warning, Error);
convert_both_enums!(PlanFault;
    ClustersOutOfRange {step}, LengthMismatch {length, steps}, ValidLengthPastEnd {valid_length, length},
);

convert_both_structs!(Extent; offset, length);
convert_both_structs!(ScanReport; candidates, unreadable: convert_vec, stopped_at);
convert_both_structs!(DamageReport; reported, stopped);
convert_both_structs!(ClusterLayout; first, count, size, offset);
convert_both_structs!(PlanMetadata; name, created, modified, accessed);
convert_both_structs!(PlanOverlap; first, second, source: Into::into);
convert_both_structs!(PlanCheck; length, valid_length, overlaps: convert_vec);
convert_struct!(MaterializeReport => shared::MaterializeReport; length, ranges: convert_vec);
convert_struct!(PlanRead => shared::PlanRead; offset, length, ranges: convert_vec);
convert_both_structs!(Tree; nodes: convert_vec);

macro_rules! convert_partial_enum {
    ($name:ident; $($variant:ident),+ $(,)?) => {
        convert_enum!($name => shared::$name; $($variant),+);
        impl TryFrom<shared::$name> for $name {
            type Error = shared::$name;

            fn try_from(value: shared::$name) -> Result<Self, Self::Error> {
                match value {
                    $(shared::$name::$variant => Ok(Self::$variant),)+
                    other => Err(other),
                }
            }
        }
    };
}

convert_partial_enum!(Feature;
    FindCandidates, WalkExtents, RebuildTree, ReportDamage, ReadExtent, Materialize,
    DeletedCandidates, DeletedExtents, ClusterSteps,
);
convert_partial_enum!(DamageKind;
    Signature, Checksum, Mismatch, Missing, Malformed, Broken, Recorded, Unreadable,
);

convert_struct!(Capabilities => shared::Capabilities; missing: convert_vec);
convert_struct!(Damage => shared::Damage; kind: Into::into, severity: Into::into,
    location: Into::into, structure, detail);

impl TryFrom<shared::Capabilities> for Capabilities {
    type Error = shared::Feature;

    fn try_from(value: shared::Capabilities) -> Result<Self, Self::Error> {
        Ok(Self {
            missing: value
                .missing
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<shared::Damage> for Damage {
    type Error = shared::DamageKind;

    fn try_from(value: shared::Damage) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: value.kind.try_into()?,
            severity: value.severity.into(),
            location: value.location.into(),
            structure: value.structure,
            detail: value.detail,
        })
    }
}

impl From<SalvageError> for shared::SalvageError {
    fn from(value: SalvageError) -> Self {
        match value {
            SalvageError::Unsupported(feature) => Self::Unsupported(feature.into()),
            SalvageError::NoGeometry => Self::NoGeometry,
            SalvageError::Bounds => Self::Bounds,
            SalvageError::Device(error) => Self::Device(error),
            SalvageError::Sink(error) => Self::Sink(error),
            SalvageError::Plan(fault) => Self::Plan(fault.into()),
        }
    }
}

impl TryFrom<shared::SalvageError> for SalvageError {
    type Error = shared::SalvageError;

    fn try_from(value: shared::SalvageError) -> Result<Self, Self::Error> {
        Ok(match value {
            shared::SalvageError::Unsupported(feature) => Self::Unsupported(
                feature
                    .try_into()
                    .map_err(shared::SalvageError::Unsupported)?,
            ),
            shared::SalvageError::NoGeometry => Self::NoGeometry,
            shared::SalvageError::Bounds => Self::Bounds,
            shared::SalvageError::Device(error) => Self::Device(error),
            shared::SalvageError::Sink(error) => Self::Sink(error),
            shared::SalvageError::Plan(fault) => Self::Plan(fault.into()),
            other => return Err(other),
        })
    }
}

macro_rules! convert_candidate {
    ($from:ident => $to:ident) => {
        impl<D> From<$from::Candidate<D>> for $to::Candidate<D> {
            fn from(value: $from::Candidate<D>) -> Self {
                Self {
                    location: value.location.into(),
                    kind: value.kind.into(),
                    state: value.state.into(),
                    integrity: value.integrity.into(),
                    name: value.name,
                    size: value.size,
                    detail: value.detail,
                }
            }
        }
    };
}

convert_candidate!(super => shared);
convert_candidate!(shared => super);

impl From<ExtentWalk> for shared::ExtentWalk {
    fn from(value: ExtentWalk) -> Self {
        Self::contiguous(convert_vec(value.extents), value.claimed, value.end.into())
    }
}

impl TryFrom<shared::ExtentWalk> for ExtentWalk {
    type Error = shared::ExtentWalk;

    fn try_from(value: shared::ExtentWalk) -> Result<Self, Self::Error> {
        let mut expected = 0;
        for extent in &value.extents {
            if extent.offset != expected || extent.uninitialized || extent.encoded.is_some() {
                return Err(value);
            }
            let Some(end) = extent.end() else {
                return Err(value);
            };
            expected = end;
        }
        if value.end == shared::WalkEnd::Complete && expected != value.claimed {
            return Err(value);
        }
        Ok(Self {
            extents: value
                .extents
                .into_iter()
                .map(|extent| extent.source.into())
                .collect(),
            claimed: value.claimed,
            end: value.end.into(),
        })
    }
}

impl From<TreeNode> for shared::TreeNode {
    fn from(value: TreeNode) -> Self {
        Self {
            candidate: value.candidate,
            parents: value
                .parents
                .into_iter()
                .map(|parent| shared::ParentClaim {
                    parent: parent.into(),
                    name: None,
                    state: shared::CandidateState::Unknown,
                })
                .collect(),
        }
    }
}

impl From<shared::TreeNode> for TreeNode {
    /// Keeps the parent IDs; the old type has no per-claim name or state.
    fn from(value: shared::TreeNode) -> Self {
        Self {
            candidate: value.candidate,
            parents: value
                .parents
                .into_iter()
                .map(|claim| claim.parent.into())
                .collect(),
        }
    }
}

impl From<MaterializedRange> for shared::MaterializedRange {
    fn from(value: MaterializedRange) -> Self {
        Self {
            output: value.output.into(),
            source: value.source.map(Into::into),
            readable: value.readable,
            encoded: false,
        }
    }
}

impl TryFrom<shared::MaterializedRange> for MaterializedRange {
    type Error = shared::MaterializedRange;

    fn try_from(value: shared::MaterializedRange) -> Result<Self, Self::Error> {
        if value.encoded {
            return Err(value);
        }
        Ok(Self {
            output: value.output.into(),
            source: value.source.map(Into::into),
            readable: value.readable,
        })
    }
}

impl TryFrom<shared::MaterializeReport> for MaterializeReport {
    type Error = shared::MaterializedRange;

    fn try_from(value: shared::MaterializeReport) -> Result<Self, Self::Error> {
        Ok(Self {
            length: value.length,
            ranges: value
                .ranges
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<shared::PlanRead> for PlanRead {
    type Error = shared::MaterializedRange;

    fn try_from(value: shared::PlanRead) -> Result<Self, Self::Error> {
        Ok(Self {
            offset: value.offset,
            length: value.length,
            ranges: value
                .ranges
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

convert_struct!(Plan => shared::Plan; steps: convert_vec, length, valid_length, metadata: Into::into);

impl From<PlanStep> for shared::PlanStep {
    fn from(value: PlanStep) -> Self {
        match value {
            PlanStep::Extent(extent) => Self::Extent(extent.into()),
            PlanStep::Zeros(length) => Self::Zeros(length),
            PlanStep::Bytes(bytes) => Self::Bytes(bytes),
            PlanStep::Clusters { first, count } => Self::Clusters { first, count },
        }
    }
}

impl TryFrom<shared::PlanStep> for PlanStep {
    type Error = shared::PlanStep;

    fn try_from(value: shared::PlanStep) -> Result<Self, Self::Error> {
        Ok(match value {
            shared::PlanStep::Extent(extent) => Self::Extent(extent.into()),
            shared::PlanStep::Zeros(length) => Self::Zeros(length),
            shared::PlanStep::Bytes(bytes) => Self::Bytes(bytes),
            shared::PlanStep::Clusters { first, count } => Self::Clusters { first, count },
            other => return Err(other),
        })
    }
}

impl TryFrom<shared::Plan> for Plan {
    type Error = shared::PlanStep;

    fn try_from(value: shared::Plan) -> Result<Self, Self::Error> {
        Ok(Self {
            steps: value
                .steps
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            length: value.length,
            valid_length: value.valid_length,
            metadata: value.metadata.into(),
        })
    }
}
