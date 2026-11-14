#![allow(dead_code)]

//! The block-cut write crash matrix (#188).
//!
//! THE MODEL. A device that persists every write in the order the writer
//! issued it, one whole volume sector at a time, loses power after some
//! number of sectors: everything before the cut is on the medium, nothing
//! after it is. [`CutDevice`] is that device. It counts sectors, not calls,
//! so a multi-sector write is cut inside itself too. Reordering inside the
//! device between flushes and a sector torn in half are not modelled.
//!
//! THE SEQUENCE. [`STEPS`] is a bounded script of every writer operation
//! (allocate and free, mkdir, create, write_file, pwrite, truncate grow and
//! shrink, rename in all its forms, times, attributes, label, unlink,
//! rmdir), each on its own mount, on a fresh guest-formatted image. One
//! uncut run counts the sectors each step writes. Then, for every cut
//! position from zero sectors to all of them, the whole sequence is
//! replayed onto a fresh copy of the image with a device that stops there.
//!
//! THE CONTRACT (the owner's decision on #188, option (b)): after a cut,
//! every file and directory the interrupted operation was not changing is
//! intact, with the same name, size, attributes, times and bytes. The
//! operation's own paths may be old, partial or missing. The volume may be
//! dirty, and the only inconsistencies allowed are ones `fsck.exfat`
//! repairs without touching anything else. A cut at a step boundary (and
//! the uncut end) leaves a clean volume, which `fsck.exfat -n` must accept
//! as it is. No atomicity is claimed.
//!
//! The guest's verdicts come from `tests/guest_support/write_cuts.rs`;
//! [`Manifest::check`] then reads the judged image with this crate's strict
//! reader.

use crate::image_support;

use fs_core::{BlockDevice, BlockRead};
use fs_exfat::interface;
use fs_exfat::timestamp::{Timestamp, Timestamps};
use fs_exfat::write::{self, SetTimes, WriteVolume};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Writer = WriteVolume<fs_core::OwnedRwSlice>;
type Change = fn(&mut Writer) -> fs_exfat::Result<()>;

/// A memory device that accepts a fixed number of sectors and then refuses
/// every later write, as if power were lost there.
#[derive(Clone)]
pub struct CutDevice {
    bytes: Arc<Mutex<Image>>,
    sector: u64,
    /// Sectors still accepted; `u64::MAX` never cuts.
    left: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
    /// The byte offset of the first sector that did not reach the device.
    refused_at: Arc<Mutex<Option<u64>>>,
}

impl CutDevice {
    pub fn new(bytes: Image, sector: u64, budget: u64) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(bytes)),
            sector,
            left: Arc::new(AtomicU64::new(budget)),
            written: Arc::new(AtomicU64::new(0)),
            refused_at: Arc::new(Mutex::new(None)),
        }
    }

    pub fn snapshot(&self) -> Image {
        self.bytes.lock().unwrap().clone()
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::SeqCst)
    }

    pub fn refused_at(&self) -> Option<u64> {
        *self.refused_at.lock().unwrap()
    }
}

impl BlockRead for CutDevice {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.bytes.lock().unwrap().read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.bytes.lock().unwrap().size_bytes()
    }
}

impl BlockDevice for CutDevice {
    fn write_at(&self, offset: u64, data: &[u8]) -> fs_core::Result<()> {
        assert!(
            offset.is_multiple_of(self.sector) && (data.len() as u64).is_multiple_of(self.sector),
            "the writer issued a write of {} bytes at {offset} that is not whole \
             {}-byte sectors, which the block-cut model cannot cut",
            data.len(),
            self.sector
        );
        let mut bytes = self.bytes.lock().unwrap();
        for (index, chunk) in data.chunks(self.sector as usize).enumerate() {
            let at = offset + index as u64 * self.sector;
            let left = self.left.load(Ordering::SeqCst);
            if left == 0 {
                self.refused_at.lock().unwrap().get_or_insert(at);
                return Err(fs_core::Error::Custom(
                    "write cut off (block-cut model)".into(),
                ));
            }
            if left != u64::MAX {
                self.left.store(left - 1, Ordering::SeqCst);
            }
            bytes.write(at, chunk)?;
            self.written.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

/// One operation of the script, on a mount of its own, and the paths it
/// changes. Those paths (and, for a directory, everything under it) may be
/// old, partial or missing after a cut; their parents' own times may
/// change. Everything else must be intact.
pub struct Step {
    pub name: &'static str,
    pub changes: &'static [&'static str],
    change: Change,
}

macro_rules! step {
    ($name:literal, [$($path:literal),*], $change:expr) => {
        Step { name: $name, changes: &[$($path),*], change: $change }
    };
}

fn cluster(volume: &Writer) -> usize {
    volume.reader().boot().cluster_size() as usize
}

fn length(volume: &Writer, path: &str) -> u64 {
    let found = volume.reader().lookup(path).unwrap();
    found.entry().unwrap().stream().data_length()
}

pub fn pattern(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2).wrapping_add(1);
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn time(milliseconds: i64) -> Timestamp {
    Timestamp::from_unix_milliseconds(milliseconds, 60).unwrap()
}

/// The script. Every supported writer operation appears at least once; the
/// files the early steps make are the untouched files the later steps'
/// cuts must preserve, beside the kernel-made `/keep/sentinel.bin`.
pub const STEPS: [Step; 22] = [
    step!("allocate-free", [], |volume| {
        let mut held = volume.allocate(2)?;
        volume.extend(&mut held, 1)?;
        volume.free(held)
    }),
    step!("mkdir", ["/Cut"], |volume| volume.mkdir("/Cut")),
    step!("mkdir-nested", ["/Cut/Dir"], |volume| volume
        .mkdir("/Cut/Dir")),
    step!("create-empty", ["/Cut/empty.txt"], |volume| volume
        .create("/Cut/empty.txt")),
    step!("write-file", ["/Cut/a.bin"], |volume| {
        let size = cluster(volume) + 700;
        volume.write_file("/Cut/a.bin", &pattern(1, size))
    }),
    step!("write-file-nested", ["/Cut/Dir/b.bin"], |volume| {
        let size = cluster(volume) / 2;
        volume.write_file("/Cut/Dir/b.bin", &pattern(2, size))
    }),
    step!("write-file-third", ["/Cut/c.bin"], |volume| {
        volume.write_file("/Cut/c.bin", &pattern(3, 1000))
    }),
    step!("pwrite-append", ["/Cut/a.bin"], |volume| {
        let end = length(volume, "/Cut/a.bin");
        let size = cluster(volume) + 3;
        volume.pwrite("/Cut/a.bin", end, &pattern(4, size))
    }),
    step!("pwrite-overwrite", ["/Cut/a.bin"], |volume| {
        volume.pwrite("/Cut/a.bin", 100, &pattern(5, 300))
    }),
    step!("pwrite-fragment", ["/Cut/Dir/b.bin"], |volume| {
        // b.bin's clusters are followed by c.bin's, so growing it past
        // its cluster makes it a FAT chain.
        let end = length(volume, "/Cut/Dir/b.bin");
        let size = cluster(volume);
        volume.pwrite("/Cut/Dir/b.bin", end, &pattern(6, size))
    }),
    step!("truncate-grow", ["/Cut/c.bin"], |volume| {
        let size = cluster(volume) as u64 * 2 + 5;
        volume.truncate("/Cut/c.bin", size)
    }),
    step!("truncate-shrink", ["/Cut/a.bin"], |volume| volume
        .truncate("/Cut/a.bin", 100)),
    step!(
        "rename-cross-directory",
        ["/Cut/empty.txt", "/Cut/Dir/moved.txt"],
        |volume| volume.rename("/Cut/empty.txt", "/Cut/Dir/moved.txt")
    ),
    step!("rename-case-only", ["/Cut/c.bin", "/Cut/C.BIN"], |volume| {
        volume.rename("/Cut/c.bin", "/Cut/C.BIN")
    }),
    step!("rename-replacing", ["/Cut/C.BIN", "/Cut/a.bin"], |volume| {
        volume.rename("/Cut/C.BIN", "/Cut/a.bin")
    }),
    step!("rename-directory", ["/Cut/Dir", "/Cut/Moved"], |volume| {
        volume.rename("/Cut/Dir", "/Cut/Moved")
    }),
    step!("set-times", ["/Cut/a.bin"], |volume| {
        let times = SetTimes {
            created: Some(time(1_111_111_111_110)),
            modified: Some(time(1_222_222_222_220)),
            accessed: Some(time(1_333_333_334_000)),
        };
        volume.set_times("/Cut/a.bin", times)
    }),
    step!("set-attributes", ["/Cut/Moved/b.bin"], |volume| {
        volume.set_attributes("/Cut/Moved/b.bin", 0x21)
    }),
    step!("set-label", [], |volume| volume.set_label("CUTS")),
    step!("unlink", ["/Cut/Moved/b.bin"], |volume| volume
        .unlink("/Cut/Moved/b.bin")),
    step!("unlink-empty", ["/Cut/Moved/moved.txt"], |volume| volume
        .unlink("/Cut/Moved/moved.txt")),
    step!("rmdir", ["/Cut/Moved"], |volume| volume.rmdir("/Cut/Moved")),
];

fn run_step(device: &CutDevice, step: &Step) -> fs_exfat::Result<()> {
    let mut volume = write::open_device(device.clone(), 0)?;
    (step.change)(&mut volume)?;
    volume.unmount()
}

/// How many sectors each step writes, from one uncut run.
pub struct Plan {
    pub blocks: Vec<u64>,
    starts: Vec<u64>,
}

impl Plan {
    pub fn measure(base: &Image, sector: u64) -> Self {
        let device = CutDevice::new(base.clone(), sector, u64::MAX);
        let mut blocks = Vec::new();
        let mut starts = Vec::new();
        for step in &STEPS {
            let before = device.written();
            run_step(&device, step).unwrap_or_else(|error| panic!("uncut {}: {error}", step.name));
            starts.push(before);
            blocks.push(device.written() - before);
            // A mount marks the volume dirty before anything else, so no
            // step writes nothing and no two steps start at one position.
            assert!(device.written() > before, "{} wrote nothing", step.name);
        }
        let final_image = device.snapshot();
        let volume = interface::open_device(final_image, 0).unwrap();
        assert_eq!(volume.metadata().label.as_deref(), Some("CUTS"));
        Self { blocks, starts }
    }

    /// Every sector the uncut run writes; cuts run from 0 to this.
    pub fn total(&self) -> u64 {
        self.blocks.iter().sum()
    }

    /// The step a cut after `cut` sectors interrupts, and how many of that
    /// step's sectors reached the device. The uncut end is the last step,
    /// complete.
    pub fn locate(&self, cut: u64) -> (usize, u64) {
        let index = match self.starts.iter().rposition(|start| *start <= cut) {
            Some(index) if cut < self.total() => index,
            _ => STEPS.len() - 1,
        };
        (index, cut - self.starts[index])
    }
}

/// One cut: the partial image and where it was cut.
pub struct Cut {
    pub cut: u64,
    pub step: usize,
    /// Sectors of the step that reached the device.
    pub done: u64,
    /// Where the first refused sector would have gone.
    pub refused_at: Option<u64>,
    pub image: Image,
    pub manifest: Manifest,
}

impl Cut {
    /// A cut at a step boundary, or the uncut end: a clean unmount.
    pub fn at_boundary(&self) -> bool {
        self.done == 0 || self.refused_at.is_none()
    }

    pub fn describe(&self, plan: &Plan) -> String {
        let step = &STEPS[self.step];
        let at = match self.refused_at {
            Some(offset) => format!("first lost sector at byte {offset}"),
            None => "uncut".to_owned(),
        };
        format!(
            "cut {} (step {} `{}`, {} of its {} sectors written, {at})",
            self.cut, self.step, step.name, self.done, plan.blocks[self.step]
        )
    }
}

/// Replay the script onto a fresh copy of `base`, cut after `cut` sectors.
pub fn replay(base: &Image, sector: u64, plan: &Plan, cut: u64) -> Cut {
    let (index, done) = plan.locate(cut);
    let device = CutDevice::new(base.clone(), sector, cut);
    let mut manifest = None;
    for (position, step) in STEPS.iter().enumerate().take(index + 1) {
        if position == index {
            manifest = Some(Manifest::read(&device.snapshot(), step.changes));
        }
        let result = run_step(&device, step);
        let last = position == index;
        let uncut = cut == plan.total();
        match result {
            Ok(()) if !last || uncut => {}
            Ok(()) => panic!(
                "cut {cut}: step `{}` finished although the device stopped after {cut} \
                 sectors; the writer's sector count is not deterministic",
                step.name
            ),
            // The cut: the step stops with the device's refusal.
            Err(_) if last && !uncut => {}
            Err(error) => panic!("cut {cut}: step `{}` failed early: {error}", step.name),
        }
    }
    assert_eq!(
        device.written(),
        cut,
        "cut {cut}: the replay wrote a different number of sectors than the plan"
    );
    Cut {
        cut,
        step: index,
        done,
        refused_at: device.refused_at(),
        image: device.snapshot(),
        manifest: manifest.unwrap(),
    }
}

/// A file-backed image with copy-on-write sectors for each crash cut.
#[derive(Clone)]
pub struct Image {
    base: Arc<image_support::Image>,
    sectors: Arc<BTreeMap<u64, [u8; 512]>>,
}

impl Image {
    pub fn open(path: &std::path::Path) -> Self {
        Self {
            base: Arc::new(image_support::Image::open(path)),
            sectors: Arc::new(BTreeMap::new()),
        }
    }

    fn write(&mut self, offset: u64, bytes: &[u8]) -> fs_core::Result<()> {
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.size_bytes())
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: bytes.len() as u64,
                size: self.size_bytes(),
            });
        }
        let mut position = 0;
        while position < bytes.len() {
            let at = offset + position as u64;
            let sector = at / 512 * 512;
            let within = (at % 512) as usize;
            let count = (512 - within).min(bytes.len() - position);
            let mut data = [0; 512];
            self.read_at(sector, &mut data)?;
            data[within..within + count].copy_from_slice(&bytes[position..position + count]);
            Arc::make_mut(&mut self.sectors).insert(sector, data);
            position += count;
        }
        Ok(())
    }

    pub fn store(&self, path: &std::path::Path) {
        image_support::sparse_copy_device(self, path);
    }
}

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.base.read_at(offset, output)?;
        let end = offset + output.len() as u64;
        for (&at, bytes) in self.sectors.range(offset.saturating_sub(511)..end) {
            let start = at.max(offset);
            let finish = (at + 512).min(end);
            output[(start - offset) as usize..(finish - offset) as usize]
                .copy_from_slice(&bytes[(start - at) as usize..(finish - at) as usize]);
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.base.size_bytes()
    }
}

#[derive(Debug, PartialEq)]
struct Node {
    /// The entry's own fields: name as stored, attributes, times, length
    /// and a hash of its bytes. None for the root and for a directory whose
    /// own times the operation may change (the parent of a changed path).
    fields: Option<(String, u16, Timestamps, u64, u64)>,
    /// A directory's children, except the ones the operation changes.
    children: Option<BTreeSet<String>>,
}

/// What must survive a cut: every path the step does not change.
pub struct Manifest {
    nodes: BTreeMap<String, Node>,
    changes: Vec<String>,
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

impl Manifest {
    fn changed(&self, path: &str) -> bool {
        let path = key(path);
        self.changes
            .iter()
            .any(|change| path == *change || path.starts_with(&format!("{change}/")))
    }

    fn parent_of_change(&self, path: &str) -> bool {
        let path = key(path);
        self.changes
            .iter()
            .any(|change| change.rsplit_once('/').map(|(parent, _)| parent) == Some(&path))
    }

    fn walk(&self, bytes: Image, mut visit: impl FnMut(&str, Node)) -> Result<(), fs_exfat::Error> {
        let volume = interface::open_device(bytes, 0)?;
        let mut pending = vec![(String::new(), None)];
        while let Some((path, entry)) = pending.pop() {
            let entries = match &entry {
                None => volume.root_directory()?,
                Some(entry) => volume.read_directory(entry)?,
            };
            let mut children = BTreeSet::new();
            for child in entries {
                let child_path = format!("{path}/{}", child.name());
                if self.changed(&child_path) {
                    continue;
                }
                children.insert(child.name().to_owned());
                if child.is_directory() {
                    pending.push((child_path, Some(child)));
                    continue;
                }
                let file = volume.open_file(&child)?;
                let mut content = vec![0; file.len() as usize];
                let read = file.read_at(0, &mut content)?;
                if read != content.len() {
                    return Err(fs_exfat::Error::Invalid("short read"));
                }
                let fields = (
                    child.name().to_owned(),
                    child.attributes(),
                    *child.timestamps(),
                    content.len() as u64,
                    fnv(&content),
                );
                visit(
                    &child_path,
                    Node {
                        fields: Some(fields),
                        children: None,
                    },
                );
            }
            let fields = match &entry {
                Some(entry) if !self.parent_of_change(&path) => Some((
                    entry.name().to_owned(),
                    entry.attributes(),
                    *entry.timestamps(),
                    entry.stream().data_length(),
                    0,
                )),
                _ => None,
            };
            visit(
                &path,
                Node {
                    fields,
                    children: Some(children),
                },
            );
        }
        Ok(())
    }

    /// The tree before the step, without the paths the step changes.
    pub fn read(image: &Image, changes: &[&str]) -> Self {
        let mut manifest = Self {
            nodes: BTreeMap::new(),
            changes: changes.iter().map(|change| key(change)).collect(),
        };
        let mut nodes = BTreeMap::new();
        manifest
            .walk(image.clone(), |path, node| {
                nodes.insert(path.to_owned(), node);
            })
            .unwrap_or_else(|error| panic!("the strict reader refuses a step's start: {error}"));
        manifest.nodes = nodes;
        manifest
    }

    /// Every unchanged path, read from `image` with the strict reader, must
    /// equal what was recorded; nothing may be added beside them.
    pub fn check(&self, image: Image) -> Result<(), String> {
        let mut found = BTreeMap::new();
        self.walk(image, |path, node| {
            found.insert(path.to_owned(), node);
        })
        .map_err(|error| format!("the strict reader refuses the volume: {error}"))?;
        let mut problems = Vec::new();
        for (path, node) in &self.nodes {
            match found.get(path) {
                None => problems.push(format!("{path:?} is missing")),
                Some(other) if other != node => {
                    problems.push(format!("{path:?} changed: {node:?} became {other:?}"))
                }
                Some(_) => {}
            }
        }
        for path in found.keys().filter(|path| !self.nodes.contains_key(*path)) {
            problems.push(format!("{path:?} appeared"));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

/// The sectors where `next` differs from `previous`.
pub fn delta(previous: &Image, next: &Image, sector: usize) -> Vec<(u64, Vec<u8>)> {
    assert_eq!(previous.size_bytes(), next.size_bytes());
    assert!(image_support::CHUNK.is_multiple_of(sector));
    let mut left = vec![0; image_support::CHUNK];
    let mut right = vec![0; image_support::CHUNK];
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < previous.size_bytes() {
        let count = (previous.size_bytes() - offset).min(left.len() as u64) as usize;
        previous.read_at(offset, &mut left[..count]).unwrap();
        next.read_at(offset, &mut right[..count]).unwrap();
        for (index, (old, new)) in left[..count]
            .chunks(sector)
            .zip(right[..count].chunks(sector))
            .enumerate()
        {
            if old != new {
                result.push((offset + (index * sector) as u64, new.to_vec()));
            }
        }
        offset += count as u64;
    }
    result
}

pub fn apply(image: &mut Image, changes: &[(u64, Vec<u8>)]) {
    for (offset, bytes) in changes {
        image.write(*offset, bytes).unwrap();
    }
}

/// This crate's allocation audit of a judged image: every cluster a stream
/// uses must be allocated and used once. Clusters allocated and used by
/// nothing are leaked space, which the contract allows; their count is
/// returned.
pub fn audit(image: Image) -> Result<u64, String> {
    let volume = interface::open_device(image, 0)
        .map_err(|error| format!("the strict reader refuses the volume: {error}"))?;
    let report = fs_exfat::check::check(&volume)
        .map_err(|error| format!("the allocation audit refuses the volume: {error}"))?;
    Ok(report.unreferenced_clusters)
}

impl Manifest {
    pub fn contains(&self, path: &str) -> bool {
        self.nodes.contains_key(path)
    }
}

/// The control that keeps the matrix from passing vacuously: the same
/// check must refuse `image` once one byte of the kernel-made sentinel's
/// data, which no step touches, is changed.
pub fn assert_check_sees_a_changed_sentinel(manifest: &Manifest, image: &Image) {
    const SENTINEL: &str = "/keep/sentinel.bin";
    assert!(manifest.contains(SENTINEL), "the manifest omits {SENTINEL}");
    let mut changed = image.clone();
    let at = {
        let volume = interface::open_device(image.clone(), 0).unwrap();
        let found = volume.lookup(SENTINEL).unwrap();
        let first = found.entry().unwrap().stream().first_cluster();
        volume.boot().cluster_offset(first).unwrap() as usize
    };
    let mut byte = [0];
    changed.read_at(at as u64, &mut byte).unwrap();
    byte[0] ^= 0xff;
    changed.write(at as u64, &byte).unwrap();
    let refused = manifest
        .check(changed)
        .expect_err("the contract check accepts a changed byte in an untouched file");
    assert!(refused.contains(SENTINEL), "{refused}");
}
