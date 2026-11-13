use fs_core::{BlockDevice, BlockRead};
use fs_exfat::check::check;
use fs_exfat::path::Node;
use fs_exfat::timestamp::Timestamp;
use fs_exfat::volume::{ReadLimits, Volume};
use fs_exfat::write::{Allocation, SetTimes, WriteVolume};
use fs_exfat::{Error, Result};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

#[path = "../../tests/write_support/mod.rs"]
mod support;
use support::{format, Geometry, Layout, Memory};

pub const WRITE_STEPS: usize = 32;
pub const WRITE_INPUT_BYTES: usize = 4 + WRITE_STEPS * 9;
const IMAGE_BYTES: usize = 1024 * 1024;
const PAYLOAD_LENGTHS: [usize; 8] = [0, 1, 511, 512, 4095, 4096, 4097, 8192];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct WriteFindings {
    pub succeeded: u16,
    pub refused: u16,
    pub injected: usize,
    pub mount_failed: bool,
    pub poisoned: bool,
    pub clean: bool,
}

impl WriteFindings {
    fn record(&mut self, opcode: u8, result: &Result<()>) {
        if result.is_ok() {
            self.succeeded |= 1 << opcode;
        } else {
            self.refused |= 1 << opcode;
        }
    }
}

fn fresh(selector: u8) -> (Memory, &'static Geometry, &'static [u8]) {
    static IMAGES: OnceLock<Vec<(Vec<u8>, Geometry)>> = OnceLock::new();
    let images = IMAGES.get_or_init(|| {
        (0..4)
            .map(|index| {
                format(&Layout {
                    sector_shift: if index & 1 == 0 { 9 } else { 12 },
                    cluster_shift: if index & 1 == 0 { 3 } else { 0 },
                    fats: 1 + (index >> 1),
                    bytes: IMAGE_BYTES as u64,
                })
            })
            .collect()
    });
    let (image, geometry) = &images[usize::from(selector & 3)];
    (
        Memory::new(image.clone()).with_block(geometry.sector as u64),
        geometry,
        image,
    )
}

fn limits() -> ReadLimits {
    ReadLimits {
        max_metadata_bytes: IMAGE_BYTES,
        max_chain_clusters: IMAGE_BYTES / 4096,
    }
}

fn commands(data: &[u8], header: usize, width: usize) -> impl Iterator<Item = &[u8]> {
    data.get(header..data.len().min(WRITE_INPUT_BYTES))
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
        .flat_map(move |line| line.chunks_exact(width))
        .take(WRITE_STEPS)
}

fn clean(device: &Memory, geometry: &Geometry, original: &[u8]) -> Volume<Memory> {
    let image = device.snapshot();
    assert_eq!(image.len(), IMAGE_BYTES);
    let backup = geometry.sector * 12..geometry.sector * 24;
    assert_eq!(image[backup.clone()], original[backup]);
    for copy in 1..usize::from(geometry.fats) {
        assert_eq!(
            geometry.fat_bytes(&image, 0),
            geometry.fat_bytes(&image, copy)
        );
        assert_eq!(
            geometry.bitmap_bytes(&image, 0),
            geometry.bitmap_bytes(&image, copy)
        );
    }
    let reader = Volume::open_with_limits(device.clone(), limits()).unwrap();
    let report = check(&reader).unwrap();
    assert!(report.healthy(), "{report:?}");
    assert_eq!(reader.boot().volume_flags & 2, 0);
    assert_eq!(
        u64::from(reader.boot().percent_in_use),
        report.allocated_clusters * 100 / u64::from(reader.boot().cluster_count)
    );
    reader
}

pub fn write_allocations(data: &[u8]) -> bool {
    write_allocation_findings(data).succeeded != 0
}

pub fn write_allocation_findings(data: &[u8]) -> WriteFindings {
    let (device, geometry, original) = fresh(data.first().copied().unwrap_or(0));
    let mut volume = WriteVolume::mount_with_limits(device.clone(), limits()).unwrap();
    let initial_free = volume.free_clusters();
    let mut held = [Allocation::EMPTY; 8];
    let mut findings = WriteFindings::default();
    for command in commands(data, 1, 4) {
        let opcode = command[0] % 4;
        let allocation = &mut held[usize::from(command[1] % 8)];
        let free = volume.free_clusters();
        let count =
            [0, 1, 2, 3, 8, free as u32, free as u32 + 1, u32::MAX][usize::from(command[2] % 8)];
        let before = *allocation;
        let result = match opcode {
            0 => volume.free(*allocation).and_then(|()| {
                *allocation = Allocation::EMPTY;
                volume.allocate(count).map(|new| *allocation = new)
            }),
            1 => volume.extend(allocation, count),
            2 => volume.shrink(allocation, count),
            _ => volume
                .free(*allocation)
                .map(|()| *allocation = Allocation::EMPTY),
        };
        findings.record(opcode, &result);
        assert!(!matches!(result, Err(Error::Device(_))), "{result:?}");
        if result.is_ok() {
            let expected = match opcode {
                0 => count,
                1 => before.count().checked_add(count).unwrap(),
                2 => before.count().min(count),
                _ => 0,
            };
            assert_eq!(allocation.count(), expected);
        }
        if result.is_err() && opcode != 0 {
            assert_eq!(*allocation, before);
        }
        let mut owners = HashSet::new();
        for allocation in &held {
            let clusters = volume.clusters(allocation).unwrap();
            assert_eq!(clusters.len(), allocation.count() as usize);
            for cluster in clusters {
                assert!((2..geometry.cluster_count + 2).contains(&cluster));
                assert!(owners.insert(cluster), "allocations overlap");
                assert!(volume.reader().bitmap().allocated(cluster).unwrap());
            }
        }
        assert_eq!(initial_free - volume.free_clusters(), owners.len() as u64);
    }
    for allocation in held {
        volume.free(allocation).unwrap();
    }
    assert_eq!(volume.free_clusters(), initial_free);
    volume.unmount().unwrap();
    clean(&device, geometry, original);
    findings.clean = true;
    findings
}

fn path(selector: u8) -> String {
    if selector & 0x80 != 0 {
        return if selector & 1 == 0 { "/" } else { "" }.into();
    }
    match selector % 16 {
        14 => format!("/{}", "x".repeat(255)),
        15 => format!("/{}", "x".repeat(256)),
        index => [
            "/a",
            "/b",
            "/d",
            "/e",
            "/d/a",
            "/d/b",
            "/e/a",
            "/d/sub",
            "/d/sub/a",
            "/é",
            "/😀",
            "/bad?",
            "/bad\0",
            "/missing/a",
        ][usize::from(index)]
        .into(),
    }
}

fn payload(command: &[u8]) -> Vec<u8> {
    vec![command[6]; PAYLOAD_LENGTHS[usize::from(command[3] % 8)]]
}

fn offset(command: &[u8]) -> u64 {
    [
        0,
        1,
        511,
        4095,
        4096,
        8191,
        16383,
        32768,
        2 * IMAGE_BYTES as u64,
        u64::MAX,
    ][usize::from(command[4] % 10)]
}

fn length(command: &[u8]) -> u64 {
    if command[4] & 0x80 != 0 {
        return u64::MAX;
    }
    [0, 1, 511, 4095, 4096, 8193, 32768, 2 * IMAGE_BYTES as u64][usize::from(command[3] % 8)]
}

fn attributes(command: &[u8]) -> u16 {
    [0, 1, 2, 4, 0x20, 0x27, 0x10, u16::MAX][usize::from(command[3] % 8)]
}

fn label(command: &[u8]) -> &'static str {
    [
        "",
        "FUZZ",
        "é",
        "😀",
        "ELEVEN12345",
        "TWELVE123456",
        "bad?",
        "bad\0",
    ][usize::from(command[3] % 8)]
}

fn times(command: &[u8]) -> SetTimes {
    let time = Timestamp {
        year: 2024,
        month: if command[3] & 1 == 0 { 2 } else { 0 },
        day: 29,
        hour: command[6] % 24,
        minute: 10,
        second: 11,
        millisecond: 120,
        utc_offset_minutes: Some(
            [0, 60, -60, 945, -960, 1, 960, -975][usize::from(command[5] % 8)],
        ),
    };
    let fields = 1 + command[4] % 7;
    SetTimes {
        created: (fields & 1 != 0).then_some(time),
        modified: (fields & 2 != 0).then_some(time),
        accessed: (fields & 4 != 0).then_some(Timestamp {
            second: time.second & !1,
            millisecond: 0,
            ..time
        }),
    }
}

fn perform<Device: BlockDevice>(volume: &mut WriteVolume<Device>, command: &[u8]) -> Result<()> {
    let path = path(command[1]);
    match command[0] % 12 {
        0 => volume.create(&path),
        1 => volume.mkdir(&path),
        2 => volume.pwrite(&path, offset(command), &payload(command)),
        3 => volume.write_file(&path, &payload(command)),
        4 => volume.truncate(&path, length(command)),
        5 => volume.unlink(&path),
        6 => volume.rmdir(&path),
        7 => volume.rename(&path, &self::path(command[2])),
        8 => volume.set_times(&path, times(command)),
        9 => volume.set_attributes(&path, attributes(command)),
        10 => volume.set_label(label(command)),
        _ => volume.flush(),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Model {
    bytes: Option<Vec<u8>>,
    attributes: u16,
}

fn tree<Device: BlockRead>(reader: &Volume<Device>) -> BTreeMap<String, Model> {
    let mut pending = vec![(String::new(), reader.root_directory().unwrap())];
    let mut found = BTreeMap::new();
    while let Some((prefix, entries)) = pending.pop() {
        for entry in entries {
            let path = format!("{prefix}/{}", entry.name()).to_ascii_lowercase();
            let bytes = if entry.is_directory() {
                pending.push((path.clone(), reader.read_directory(&entry).unwrap()));
                None
            } else {
                let file = reader.open_file(&entry).unwrap();
                assert!(file.len() <= IMAGE_BYTES as u64);
                let mut bytes = vec![0xa5; file.len() as usize];
                assert_eq!(file.read_at(0, &mut bytes).unwrap(), bytes.len());
                Some(bytes)
            };
            let previous = found.insert(
                path,
                Model {
                    bytes,
                    attributes: entry.attributes(),
                },
            );
            assert!(previous.is_none());
            assert!(found.len() <= WRITE_STEPS);
        }
    }
    found
}

pub fn write_sequence(data: &[u8]) -> bool {
    write_sequence_findings(data).succeeded != 0
}

pub fn write_sequence_findings(data: &[u8]) -> WriteFindings {
    let (device, geometry, original) = fresh(data.first().copied().unwrap_or(0));
    let mut volume = WriteVolume::mount_with_limits(device.clone(), limits()).unwrap();
    let mut model = BTreeMap::<String, Model>::new();
    let mut expected_label = String::from("WRITER");
    let mut findings = WriteFindings::default();
    for command in commands(data, 1, 8) {
        let opcode = command[0] % 12;
        let result = perform(&mut volume, command);
        assert!(!matches!(result, Err(Error::Device(_))), "{result:?}");
        findings.record(opcode, &result);
        if result.is_err() {
            continue;
        }
        let path = path(command[1]).to_ascii_lowercase();
        match opcode {
            0 | 1 => {
                assert!(model
                    .insert(
                        path,
                        Model {
                            bytes: (opcode == 0).then(Vec::new),
                            attributes: if opcode == 0 { 0x20 } else { 0x10 },
                        }
                    )
                    .is_none());
            }
            2..=4 => {
                let file = model.entry(path).or_insert_with(|| Model {
                    bytes: Some(Vec::new()),
                    attributes: 0x20,
                });
                let bytes = file.bytes.as_mut().unwrap();
                match opcode {
                    2 => {
                        let payload = payload(command);
                        if !payload.is_empty() {
                            let offset = usize::try_from(offset(command)).unwrap();
                            let end = offset.checked_add(payload.len()).unwrap();
                            assert!(end <= IMAGE_BYTES);
                            bytes.resize(bytes.len().max(end), 0);
                            bytes[offset..end].copy_from_slice(&payload);
                        }
                    }
                    3 => *bytes = payload(command),
                    _ => {
                        let length = usize::try_from(length(command)).unwrap();
                        assert!(length <= IMAGE_BYTES);
                        bytes.resize(length, 0);
                    }
                }
                file.attributes |= 0x20;
            }
            5 | 6 => {
                assert!(model.remove(&path).is_some());
            }
            7 => {
                let destination = self::path(command[2]).to_ascii_lowercase();
                let moved: Vec<_> = model
                    .keys()
                    .filter(|key| **key == path || key.starts_with(&format!("{path}/")))
                    .cloned()
                    .collect();
                let moved: Vec<_> = moved
                    .into_iter()
                    .map(|key| {
                        let value = model.remove(&key).unwrap();
                        (format!("{destination}{}", &key[path.len()..]), value)
                    })
                    .collect();
                model.extend(moved);
            }
            8 => {
                let Node::Entry(entry) = volume.reader().lookup(&path).unwrap() else {
                    panic!("times on root")
                };
                let expected = times(command);
                let actual = entry.timestamps();
                for (expected, actual) in [
                    (expected.created, actual.created),
                    (expected.modified, actual.modified),
                    (expected.accessed, actual.accessed),
                ] {
                    if let Some(expected) = expected {
                        assert_eq!(actual, expected);
                    }
                }
            }
            9 => {
                let entry = model.get_mut(&path).unwrap();
                entry.attributes = entry.attributes & 0x10 | attributes(command);
            }
            10 => expected_label = label(command).into(),
            _ => {}
        }
    }
    let result = volume.unmount();
    if result.is_ok() {
        let reader = clean(&device, geometry, original);
        assert_eq!(tree(&reader), model);
        assert_eq!(
            reader.metadata().label.as_deref(),
            Some(expected_label.as_str())
        );
        findings.clean = true;
    } else {
        assert_ne!(device.snapshot()[106] & 2, 0);
    }
    findings
}

#[derive(Default)]
struct Fault {
    armed: bool,
    kind: u8,
    remaining: usize,
    hits: usize,
}

#[derive(Clone)]
struct Failing {
    memory: Memory,
    fault: Arc<Mutex<Fault>>,
}

impl Failing {
    fn access(&self, kind: u8) -> fs_core::Result<()> {
        let mut fault = self.fault.lock().unwrap();
        if fault.armed && fault.kind == kind {
            if fault.remaining == 0 {
                fault.hits += 1;
                return Err(fs_core::Error::Custom("injected writer I/O failure".into()));
            }
            fault.remaining -= 1;
        }
        Ok(())
    }
}

impl BlockRead for Failing {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.access(0)?;
        self.memory.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.memory.size_bytes()
    }
}

impl BlockDevice for Failing {
    fn write_at(&self, offset: u64, data: &[u8]) -> fs_core::Result<()> {
        self.access(1)?;
        self.memory.write_at(offset, data)
    }

    fn flush(&self) -> fs_core::Result<()> {
        self.access(2)
    }
    fn is_writable(&self) -> bool {
        true
    }
}

pub fn write_failures(data: &[u8]) -> bool {
    write_failure_findings(data).injected != 0
}

pub fn write_failure_findings(data: &[u8]) -> WriteFindings {
    if data.len() < 4 {
        return WriteFindings::default();
    }
    let (memory, geometry, original) = fresh(data[0]);
    let fault = Arc::new(Mutex::new(Fault {
        armed: data[3] & 1 != 0,
        kind: data[1] % 3,
        remaining: usize::from(data[2] & 15),
        hits: 0,
    }));
    let device = Failing {
        memory: memory.clone(),
        fault: fault.clone(),
    };
    let mut findings = WriteFindings::default();
    let mut volume = match WriteVolume::mount_with_limits(device, limits()) {
        Ok(volume) => volume,
        Err(error) => {
            findings.injected = fault.lock().unwrap().hits;
            findings.mount_failed = true;
            assert!(findings.injected > 0);
            assert!(matches!(error, Error::Device(_)), "{error:?}");
            return findings;
        }
    };
    fault.lock().unwrap().armed = false;
    volume.write_file("/a", &[0xa5; 8192]).unwrap();
    volume.mkdir("/d").unwrap();
    let mut allocation = volume.allocate(8).unwrap();
    let mut extras = Vec::new();
    fault.lock().unwrap().armed = true;
    for command in commands(data, 4, 8) {
        let opcode = command[0] % 16;
        let hits_before = fault.lock().unwrap().hits;
        let result = match opcode {
            12 => volume
                .allocate(u32::from(command[3] % 8))
                .map(|new| extras.push(new)),
            13 => volume.extend(&mut allocation, u32::from(command[3] % 8)),
            14 => volume.shrink(&mut allocation, u32::from(command[3] % 8)),
            15 => volume
                .free(allocation)
                .map(|()| allocation = Allocation::EMPTY),
            _ => {
                let mut command: [u8; 8] = command.try_into().unwrap();
                command[0] = opcode;
                perform(&mut volume, &command)
            }
        };
        findings.record(opcode, &result);
        if fault.lock().unwrap().hits > hits_before {
            assert!(matches!(result, Err(Error::Device(_))), "{result:?}");
        }
        if result.is_err() {
            break;
        }
    }
    fault.lock().unwrap().armed = false;
    findings.injected = fault.lock().unwrap().hits;
    let before = memory.snapshot();
    let probe = volume.set_label("PROBE");
    if let Err(error) = probe {
        findings.poisoned = true;
        assert!(
            matches!(error, Error::Invalid(reason) if reason.contains("earlier write failed")),
            "{error:?}"
        );
        assert_eq!(memory.snapshot(), before);
        assert!(volume.set_label("REFUSED").is_err());
        assert_eq!(memory.snapshot(), before);
        assert!(volume.unmount().is_err());
        assert_ne!(memory.snapshot()[106] & 2, 0);
    } else {
        volume.free(allocation).unwrap();
        for allocation in extras {
            volume.free(allocation).unwrap();
        }
        volume.unmount().unwrap();
        clean(&memory, geometry, original);
        findings.clean = true;
    }
    findings
}
