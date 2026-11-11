//! The kernel-made fixtures (`chore fixtures`) and the three readings the
//! tiers compare: the kernel's listing captured in the guest when the image
//! was built, this crate's strict reader, and its salvage verbs.
//!
//! NOTHING SKIPS. A missing image, manifest or listing fails the test that
//! wanted it, naming `chore fixtures`, which builds them in the harness VM.
//! There is one way to reach a fixture, [`fixture`], and nowhere to put an
//! early return.
//!
//! THE LISTING FORMAT is `test-disks/fixture-tool.py`'s: one tab-separated
//! line per entry, `type size mode mtime_ns atime_ns sha256 path`, sorted by
//! path, written from a read-only kernel mount with `fmask=0,dmask=0`, so the
//! mode's write bits say only whether the read-only attribute is set.

#![allow(dead_code)]

use fs_core::BlockRead;
use fs_exfat::directory::DirectoryEntry;
use fs_exfat::salvage::{
    Candidate, CandidateKind, CandidateState, EntrySetDetail, ExfatSalvage, Extent, Integrity,
    Parent, SalvageAdapter, WalkEnd,
};
use fs_exfat::timestamp::Timestamp;
use fs_exfat::volume::Volume;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The read-only attribute (exFAT FileAttributes bit 0).
const READ_ONLY: u16 = 0x01;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every fixture image's file name, from `build-fixtures.sh --list`: the one
/// list the builder, `chore fixtures` and the tests share.
pub fn images() -> Vec<String> {
    let out = Command::new("bash")
        .arg(repo().join("test-disks/build-fixtures.sh"))
        .arg("--list")
        .output()
        .expect("bash runs test-disks/build-fixtures.sh --list");
    assert!(out.status.success(), "build-fixtures.sh --list failed");
    let images: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(images.len() >= 10, "the fixture list shrank: {images:?}");
    images
}

/// A fixture file under test-disks/, which must exist.
#[track_caller]
pub fn fixture(name: &str) -> PathBuf {
    let path = repo().join("test-disks").join(name);
    assert!(
        path.is_file(),
        "{} is missing. The fixtures are made by the Linux kernel in the harness VM: \
         run `chore fixtures` (or download CI's `fixtures` artifact into test-disks/). \
         Tests never skip on a missing fixture.",
        path.display()
    );
    path
}

/// `<image>.manifest`: how the image was made, as `key: value` lines.
pub struct Manifest(BTreeMap<String, String>);

impl Manifest {
    #[track_caller]
    pub fn get(&self, key: &str) -> &str {
        self.0
            .get(key)
            .unwrap_or_else(|| panic!("the manifest has no `{key}`"))
    }

    pub fn find(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// `512`, `4K`, `32M` and the like, in bytes.
    #[track_caller]
    pub fn bytes(&self, key: &str) -> u64 {
        let value = self.get(key);
        let (digits, scale) = match value.as_bytes().last() {
            Some(b'K') => (&value[..value.len() - 1], 1 << 10),
            Some(b'M') => (&value[..value.len() - 1], 1 << 20),
            _ => (value, 1),
        };
        digits.parse::<u64>().unwrap() * scale
    }

    pub fn damaged(&self) -> bool {
        self.find("damage").is_some()
    }
}

pub fn manifest(image: &str) -> Manifest {
    let text = std::fs::read_to_string(fixture(&format!("{image}.manifest"))).unwrap();
    Manifest(
        text.lines()
            .filter_map(|line| line.split_once(": "))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
}

/// One entry, as every reading reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub directory: bool,
    pub size: u64,
    pub read_only: bool,
    pub modified_ms: i64,
    pub accessed_ms: i64,
    /// None for a directory.
    pub sha256: Option<String>,
}

impl fmt::Display for Row {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} size={} read_only={} modified_ms={} accessed_ms={} sha256={}",
            if self.directory { "dir" } else { "file" },
            self.size,
            self.read_only,
            self.modified_ms,
            self.accessed_ms,
            self.sha256.as_deref().unwrap_or("-")
        )
    }
}

pub type Listing = BTreeMap<String, Row>;

/// A listing file written by fixture-tool.py.
pub fn read_listing(path: &Path) -> Listing {
    let text = std::fs::read_to_string(path).unwrap();
    let mut rows = Listing::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.splitn(7, '\t').collect();
        assert_eq!(
            fields.len(),
            7,
            "malformed listing line in {}",
            path.display()
        );
        let nanoseconds = |field: &str| -> i64 {
            let value: i64 = field.parse().unwrap();
            assert_eq!(
                value % 1_000_000,
                0,
                "exFAT records at most 10 ms precision"
            );
            value / 1_000_000
        };
        let mode = u32::from_str_radix(fields[2], 8).unwrap();
        let row = Row {
            directory: fields[0] == "d",
            size: fields[1].parse().unwrap(),
            read_only: mode & 0o222 == 0,
            modified_ms: nanoseconds(fields[3]),
            accessed_ms: nanoseconds(fields[4]),
            sha256: (fields[5] != "-").then(|| fields[5].to_owned()),
        };
        assert!(rows.insert(fields[6].to_owned(), row).is_none());
    }
    rows
}

/// The kernel's listing of `image`, captured in the guest when it was built.
pub fn kernel_listing(image: &str) -> Listing {
    read_listing(&fixture(&format!("{image}.listing.tsv")))
}

/// Milliseconds since the epoch. A timestamp without a valid UTC offset is
/// read as UTC, as the guest (TZ=UTC) wrote and reads it.
pub fn milliseconds(timestamp: &Timestamp) -> i64 {
    timestamp.unix_milliseconds().unwrap_or_else(|| {
        Timestamp {
            utc_offset_minutes: Some(0),
            ..*timestamp
        }
        .unix_milliseconds()
        .expect("a valid exFAT timestamp")
    })
}

/// This crate's strict reader's listing of a volume, in the kernel's terms.
pub fn strict_listing<Device: BlockRead>(volume: &Volume<Device>) -> Listing {
    let mut rows = Listing::new();
    let root = volume.root_directory().unwrap();
    strict_walk(volume, root, "", &mut rows);
    rows
}

fn strict_walk<Device: BlockRead>(
    volume: &Volume<Device>,
    entries: Vec<DirectoryEntry>,
    prefix: &str,
    rows: &mut Listing,
) {
    assert!(
        prefix.matches('/').count() < 64,
        "directory cycle or excessive depth"
    );
    for entry in entries {
        let path = format!("{prefix}{}", entry.name());
        let times = entry.timestamps();
        let mut row = Row {
            directory: entry.is_directory(),
            size: entry.stream().data_length(),
            read_only: entry.attributes() & READ_ONLY != 0,
            modified_ms: milliseconds(&times.modified),
            accessed_ms: milliseconds(&times.accessed),
            sha256: None,
        };
        if entry.is_directory() {
            let children = volume.read_directory(&entry).unwrap();
            assert!(rows.insert(path.clone(), row).is_none(), "{path} twice");
            strict_walk(volume, children, &format!("{path}/"), rows);
            continue;
        }
        let file = volume.open_file(&entry).unwrap();
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut buffer = vec![0; 1 << 20];
        while offset < file.len() {
            let count = file.read_at(offset, &mut buffer).unwrap();
            assert!(count > 0, "{path}: short read at {offset}");
            hash.update(&buffer[..count]);
            offset += count as u64;
        }
        row.sha256 = Some(format!("{:x}", hash.finalize()));
        assert!(rows.insert(path.clone(), row).is_none(), "{path} twice");
    }
}

/// What the salvage verbs find in a source: live entry sets placed in the
/// tree they rebuild, and deleted ones, each read back through
/// `walk_extents` and `read_extent`.
pub struct Salvaged {
    pub live: Listing,
    pub deleted: Listing,
    /// Live sets with no single parent, a failed checksum, or a broken walk.
    pub problems: Vec<String>,
}

pub fn salvage(source: &dyn BlockRead) -> Salvaged {
    let adapter = ExfatSalvage::probe(source);
    let mut candidates: Vec<Candidate<EntrySetDetail>> = Vec::new();
    adapter
        .find_candidates(
            source,
            Extent {
                offset: 0,
                length: source.size_bytes(),
            },
            &mut candidates,
        )
        .unwrap();
    candidates.retain(|candidate| {
        matches!(
            candidate.kind,
            CandidateKind::File | CandidateKind::Directory
        )
    });
    let tree = adapter.rebuild_tree(source, &candidates).unwrap();
    let by_offset: HashMap<u64, usize> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.location.offset, index))
        .collect();
    let mut problems = Vec::new();

    let path_of = |index: usize, problems: &mut Vec<String>| -> Option<String> {
        let mut parts = Vec::new();
        let mut current = index;
        for _ in 0..64 {
            let candidate = &candidates[current];
            parts.push(candidate.name.clone().unwrap_or_default());
            match tree.nodes[current].parents.as_slice() {
                [Parent::Root] => {
                    parts.reverse();
                    return Some(parts.join("/"));
                }
                [Parent::Candidate(offset)] => match by_offset.get(offset) {
                    Some(parent) => current = *parent,
                    None => break,
                },
                parents => {
                    problems.push(format!(
                        "{:?} at {}: {} parents",
                        candidate.name,
                        candidate.location.offset,
                        parents.len()
                    ));
                    return None;
                }
            }
        }
        problems.push(format!("{:?}: no path to the root", candidates[index].name));
        None
    };

    let mut live = Listing::new();
    let mut deleted = Listing::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let target = match candidate.state {
            CandidateState::Live => &mut live,
            CandidateState::Deleted => &mut deleted,
            CandidateState::Unknown => continue,
        };
        if candidate.integrity != Integrity::Verified {
            if candidate.state == CandidateState::Live {
                problems.push(format!(
                    "{:?} at {}: integrity {:?}",
                    candidate.name, candidate.location.offset, candidate.integrity
                ));
            }
            continue;
        }
        let Some(path) = path_of(index, &mut problems) else {
            continue;
        };
        let directory = candidate.kind == CandidateKind::Directory;
        let mut row = Row {
            directory,
            size: candidate.size.unwrap_or(0),
            read_only: candidate.detail.attributes & READ_ONLY != 0,
            modified_ms: 0,
            accessed_ms: 0,
            sha256: None,
        };
        if !directory {
            match read_candidate(&adapter, source, candidate) {
                Ok(digest) => row.sha256 = Some(digest),
                Err(problem) => {
                    problems.push(format!("{path}: {problem}"));
                    continue;
                }
            }
        }
        if let Some(previous) = target.insert(path.clone(), row) {
            problems.push(format!("{path}: found twice ({previous})"));
        }
    }
    Salvaged {
        live,
        deleted,
        problems,
    }
}

/// SHA-256 of a candidate's content, read through its extent walk. Bytes past
/// the valid data length are zeros, as the specification says they read.
fn read_candidate(
    adapter: &ExfatSalvage,
    source: &dyn BlockRead,
    candidate: &Candidate<EntrySetDetail>,
) -> Result<String, String> {
    let walk = adapter
        .walk_extents(source, candidate)
        .map_err(|error| format!("{error:?}"))?;
    if walk.end != WalkEnd::Complete {
        return Err(format!("walk ended {:?}", walk.end));
    }
    let size = candidate.size.unwrap_or(0);
    let valid = candidate.detail.valid_data_length.min(size);
    let mut hash = Sha256::new();
    let mut position = 0u64;
    let mut buffer = vec![0; 1 << 20];
    for extent in &walk.extents {
        let mut within = 0;
        while within < extent.length && position < size {
            let want = (extent.length - within)
                .min(buffer.len() as u64)
                .min(size - position) as usize;
            let count = adapter
                .read_extent(source, *extent, within, &mut buffer[..want])
                .map_err(|error| format!("{error:?}"))?;
            if count == 0 {
                return Err(format!("short read at {position}"));
            }
            for (index, byte) in buffer[..count].iter_mut().enumerate() {
                if position + index as u64 >= valid {
                    *byte = 0;
                }
            }
            hash.update(&buffer[..count]);
            within += count as u64;
            position += count as u64;
        }
    }
    if position != size {
        return Err(format!("extents cover {position} of {size} bytes"));
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Field-by-field differences between two listings, each naming the path
/// and the field, so a failure says what disagrees and where.
pub fn differences(
    expected_name: &str,
    expected: &Listing,
    observed_name: &str,
    observed: &Listing,
    times: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, want) in expected {
        let Some(got) = observed.get(path) else {
            out.push(format!(
                "{path}: in {expected_name}, missing from {observed_name}"
            ));
            continue;
        };
        let mut field = |name: &str, want: String, got: String| {
            if want != got {
                out.push(format!(
                    "{path}: {name} is {want} to {expected_name} and {got} to {observed_name}"
                ));
            }
        };
        field(
            "type",
            want.directory.to_string(),
            got.directory.to_string(),
        );
        field("size", want.size.to_string(), got.size.to_string());
        field(
            "read-only",
            want.read_only.to_string(),
            got.read_only.to_string(),
        );
        field(
            "sha256",
            format!("{:?}", want.sha256),
            format!("{:?}", got.sha256),
        );
        if times {
            field(
                "mtime ms",
                want.modified_ms.to_string(),
                got.modified_ms.to_string(),
            );
            field(
                "atime ms",
                want.accessed_ms.to_string(),
                got.accessed_ms.to_string(),
            );
        }
    }
    for path in observed.keys().filter(|path| !expected.contains_key(*path)) {
        out.push(format!(
            "{path}: in {observed_name}, missing from {expected_name}"
        ));
    }
    out
}

#[track_caller]
pub fn assert_same(
    image: &str,
    expected_name: &str,
    expected: &Listing,
    observed_name: &str,
    observed: &Listing,
    times: bool,
) {
    let found = differences(expected_name, expected, observed_name, observed, times);
    assert!(
        found.is_empty(),
        "{image}: {} difference(s) between {expected_name} and {observed_name}:\n  {}",
        found.len(),
        found
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}
