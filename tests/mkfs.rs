//! The formatter against this crate's strict reader, check and deviation
//! report. These prove the formatter and the reader agree; the macOS oracle in
//! `tests/oracle/mkfs.rs` is what proves they agree with anything else.

use fs_core::{BlockDevice, BlockRead, FileDevice};
use fs_exfat::boot::BootRegions;
use fs_exfat::device::Reader;
use fs_exfat::mkfs::{self, FormatError, FormatOptions, Layout};
use fs_exfat::primitives::{checksum32, BootRegion, RawUpcaseTable, UninterpretedCounts};
use fs_exfat::{check, interface};
use std::sync::{Arc, Mutex};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Write(u64, usize),
    Flush,
}

/// A writable device in memory that records every write and flush.
#[derive(Clone)]
struct Memory {
    bytes: Arc<Mutex<Vec<u8>>>,
    events: Arc<Mutex<Vec<Event>>>,
    writable: bool,
}

impl Memory {
    fn new(length: u64) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(vec![0; length as usize])),
            events: Arc::default(),
            writable: true,
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }
}

impl BlockRead for Memory {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        output.copy_from_slice(&bytes[start..start + output.len()]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }
}

impl BlockDevice for Memory {
    fn write_at(&self, offset: u64, input: &[u8]) -> fs_core::Result<()> {
        if !self.writable {
            return Err(fs_core::Error::ReadOnly);
        }
        let mut bytes = self.bytes.lock().unwrap();
        let start = offset as usize;
        bytes[start..start + input.len()].copy_from_slice(input);
        self.events
            .lock()
            .unwrap()
            .push(Event::Write(offset, input.len()));
        Ok(())
    }

    fn flush(&self) -> fs_core::Result<()> {
        self.events.lock().unwrap().push(Event::Flush);
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.writable
    }
}

/// A read-only copy of formatted bytes, for the strict reader.
struct Image(Vec<u8>);

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let start = offset as usize;
        output.copy_from_slice(&self.0[start..start + output.len()]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

fn options(sector: u32) -> FormatOptions {
    FormatOptions {
        bytes_per_sector: Some(sector),
        label: Some("MKFS".into()),
        volume_serial: Some(0x1234_ABCD),
        ..FormatOptions::default()
    }
}

/// The strict reader opens the volume, its check finds it healthy, with no
/// deviation from the specification and nothing it does not interpret, and
/// both boot copies are valid and equal.
fn assert_accepted<Device: BlockRead + 'static>(device: Device, layout: &Layout) {
    let volume = interface::open_device(device, 0).unwrap();
    let boot = volume.boot();
    assert_eq!(boot.volume_length, layout.volume_length);
    assert_eq!(boot.fat_offset, layout.fat_offset);
    assert_eq!(boot.fat_length, layout.fat_length);
    assert_eq!(boot.cluster_heap_offset, layout.cluster_heap_offset);
    assert_eq!(boot.cluster_count, layout.cluster_count);
    assert_eq!(boot.root_cluster, layout.root.first_cluster);
    assert_eq!(boot.volume_serial, layout.volume_serial);
    assert_eq!(boot.number_of_fats, layout.number_of_fats);
    assert_eq!(boot.percent_in_use, layout.percent_in_use);
    assert_eq!(boot.volume_flags, 0);
    assert!(volume.read_directory_path("/").unwrap().is_empty());
    let report = check::check(&volume).unwrap();
    assert!(report.healthy(), "{report:?}");
    assert_eq!(
        report.allocated_clusters,
        u64::from(layout.allocated_clusters())
    );
    assert_eq!(report.referenced_clusters, report.allocated_clusters);
    // The root is the one directory.
    assert_eq!((report.files, report.directories), (0, 1));
    assert!(report.boot_differences.is_empty());
    assert_eq!(report.uninterpreted, UninterpretedCounts::default());
    assert!(
        report.deviations.found.is_empty(),
        "{:?}",
        report.deviations
    );
    assert!(report.deviations.complete);
    assert!(report.deviations.omitted.is_empty());
}

#[test]
fn the_recommended_up_case_table_is_table_25() {
    let bytes = mkfs::recommended_upcase_bytes();
    assert_eq!(bytes.len(), 5836);
    assert_eq!(checksum32(&bytes), mkfs::RECOMMENDED_UPCASE_CHECKSUM);
    assert_eq!(mkfs::RECOMMENDED_UPCASE_CHECKSUM, 0xE619_D30D);
    let table = RawUpcaseTable::parse(&bytes, 0xE619_D30D);
    assert!(table.usable() && table.compressed);
    assert_eq!(table.mappings.len(), 0x1_0000);
    assert_eq!(table.mappings[usize::from(b'a')], u16::from(b'A'));
    assert_eq!(table.mappings[0x00E9], 0x00C9);
    assert_eq!(table.mappings[0xFF41], 0xFF21);
}

#[test]
fn default_cluster_sizes_follow_the_documented_steps() {
    for (bytes, sector, expected) in [
        (MIB, 512, 4 << 10),
        (256 * MIB - 1, 512, 4 << 10),
        (256 * MIB, 512, 32 << 10),
        (32 * GIB - 1, 512, 32 << 10),
        (32 * GIB, 512, 128 << 10),
        (2048 * GIB - (256 << 10), 512, 128 << 10),
        (2048 * GIB - (128 << 10), 512, 256 << 10),
        (2048 * GIB, 512, 256 << 10),
        (16384 * GIB, 512, 2 << 20),
        (u64::MAX, 512, 32 << 20),
        (MIB, 4096, 4096),
    ] {
        assert_eq!(
            mkfs::default_cluster_size(bytes, sector),
            expected,
            "{bytes}"
        );
    }
}

#[test]
fn every_sector_size_and_small_size_is_accepted_and_checks_clean() {
    for sector in [512, 1024, 2048, 4096] {
        for length in [MIB, 3 * MIB + 511, 64 * MIB, 300 * MIB] {
            let device = Memory::new(length);
            let layout = mkfs::format(&device, &options(sector)).unwrap();
            assert_eq!(layout.volume_length, length / u64::from(sector));
            assert_eq!(layout.bytes_per_sector(), u64::from(sector));
            assert_eq!(
                layout.cluster_size(),
                u64::from(mkfs::default_cluster_size(layout.volume_bytes(), sector))
            );
            assert_accepted(Image(device.snapshot()), &layout);
        }
    }
}

#[test]
fn the_fat_and_heap_start_on_the_alignment_and_the_heap_fills_the_volume() {
    for (sector, length, alignment) in [
        (512, 64 * MIB, None),
        (512, 64 * MIB, Some(65536)),
        (4096, 64 * MIB, Some(1 << 20)),
        (512, 300 * MIB, Some(4 << 20)),
        (2048, 5 * MIB, Some(6144)),
    ] {
        let options = FormatOptions {
            alignment,
            ..options(sector)
        };
        let layout = mkfs::plan(length, &options).unwrap();
        let alignment = alignment.unwrap_or(layout.cluster_size());
        let sector = layout.bytes_per_sector();
        assert_eq!(u64::from(layout.fat_offset) * sector % alignment, 0);
        assert!(u64::from(layout.fat_offset) >= 24);
        assert_eq!(
            u64::from(layout.cluster_heap_offset) * sector % alignment,
            0
        );
        let fat_needed = (u64::from(layout.cluster_count) + 2) * 4;
        assert!(u64::from(layout.fat_length) * sector >= fat_needed);
        assert!(layout.fat_offset + layout.fat_length <= layout.cluster_heap_offset);
        let heap_sectors = layout.volume_length - u64::from(layout.cluster_heap_offset);
        let per_cluster = layout.cluster_size() / sector;
        assert_eq!(u64::from(layout.cluster_count), heap_sectors / per_cluster);
        let device = Memory::new(length);
        assert_eq!(mkfs::format(&device, &options).unwrap(), layout);
        assert_accepted(Image(device.snapshot()), &layout);
    }
}

#[test]
fn a_texfat_volume_has_two_equal_fats_and_two_bitmaps() {
    for sector in [512, 4096] {
        let device = Memory::new(64 * MIB);
        let options = FormatOptions {
            number_of_fats: 2,
            ..options(sector)
        };
        let layout = mkfs::format(&device, &options).unwrap();
        assert_eq!(layout.bitmaps.len(), 2);
        let bytes = device.snapshot();
        let sector = layout.bytes_per_sector() as usize;
        let fat = |copy: usize| {
            let start = (layout.fat_offset as usize + copy * layout.fat_length as usize) * sector;
            bytes[start..start + layout.fat_length as usize * sector].to_vec()
        };
        assert_eq!(fat(0), fat(1));
        let root = mkfs::root_entries(&layout);
        assert_eq!(&root[32..34], &[0x81, 0]);
        assert_eq!(&root[64..66], &[0x81, 1]);
        assert_accepted(Image(bytes), &layout);
    }
}

#[test]
fn both_boot_regions_are_equal_and_carry_their_checksum() {
    for sector in [512usize, 4096] {
        let device = Memory::new(8 * MIB);
        mkfs::format(&device, &options(sector as u32)).unwrap();
        let bytes = device.snapshot();
        let main = &bytes[..12 * sector];
        assert_eq!(main, &bytes[12 * sector..24 * sector]);
        let region = BootRegion::parse(main, sector).unwrap();
        assert!(region.checksum_valid && region.extended_signatures_valid);
        assert!(main[120..510].iter().all(|&byte| byte == 0xF4));
        assert!(main[9 * sector..11 * sector].iter().all(|&byte| byte == 0));
        let regions = BootRegions::read(&Reader::new(Image(bytes))).unwrap();
        assert!(regions.main.valid() && regions.backup.valid());
        assert!(regions.differences.is_empty());
    }
}

#[test]
fn the_boot_sector_is_written_last_after_a_flush() {
    let device = Memory::new(8 * MIB);
    mkfs::format(&device, &options(512)).unwrap();
    let events = device.events.lock().unwrap();
    let count = events.len();
    assert_eq!(events[count - 1], Event::Flush);
    assert_eq!(events[count - 2], Event::Write(0, 512));
    assert_eq!(events[count - 3], Event::Flush);
    assert!(!events[..count - 2].contains(&Event::Write(0, 512)));
    assert!(events[..count - 2].iter().all(|event| match event {
        Event::Write(offset, _) => *offset >= 512,
        Event::Flush => true,
    }));
}

#[test]
fn a_later_format_keeps_the_oem_parameters() {
    let device = Memory::new(16 * MIB);
    let first = mkfs::format(&device, &options(512)).unwrap();
    assert!(!first.oem_parameters_preserved);
    // A Flash Parameters structure (section 3.3.4) in Parameters[0], resealed
    // in both boot regions.
    let mut parameters = [0u8; 480];
    parameters[..16].copy_from_slice(&[0x46; 16]);
    parameters[16..20].copy_from_slice(&(4u32 << 20).to_le_bytes());
    let mut oem = [0u8; 512];
    oem[..480].copy_from_slice(&parameters);
    let mut region = device.snapshot()[..12 * 512].to_vec();
    region[9 * 512..10 * 512].copy_from_slice(&oem);
    let sealed = mkfs::boot_region(&first, &parameters);
    assert_eq!(sealed[..11 * 512], region[..11 * 512]);
    device.write_at(0, &sealed).unwrap();
    device.write_at(12 * 512, &sealed).unwrap();

    let second = FormatOptions {
        cluster_size: Some(8192),
        label: None,
        ..options(512)
    };
    let layout = mkfs::format(&device, &second).unwrap();
    assert!(layout.oem_parameters_preserved);
    let bytes = device.snapshot();
    assert_eq!(bytes[9 * 512..9 * 512 + 480], parameters);
    assert_eq!(bytes[21 * 512..21 * 512 + 480], parameters);
    assert_accepted(Image(bytes), &layout);
}

#[test]
fn an_invalid_existing_boot_region_leaves_null_parameters() {
    let device = Memory::new(8 * MIB);
    let layout = mkfs::format(&device, &options(512)).unwrap();
    let mut damaged = mkfs::boot_region(&layout, &[0x55; 480]);
    damaged[9 * 512] ^= 1; // the checksum no longer matches
    device.write_at(0, &damaged).unwrap();
    device.write_at(12 * 512, &damaged).unwrap();
    let layout = mkfs::format(&device, &options(512)).unwrap();
    assert!(!layout.oem_parameters_preserved);
    assert!(device.snapshot()[9 * 512..10 * 512]
        .iter()
        .all(|&byte| byte == 0));
}

#[test]
fn the_same_options_give_the_same_bytes() {
    let first = Memory::new(32 * MIB);
    let second = Memory::new(32 * MIB);
    let options = FormatOptions {
        guid: Some(*b"0123456789abcdef"),
        partition_offset: 2048,
        ..options(512)
    };
    mkfs::format(&first, &options).unwrap();
    mkfs::format(&second, &options).unwrap();
    assert_eq!(first.snapshot(), second.snapshot());
    let volume = interface::open_device(Image(first.snapshot()), 0).unwrap();
    assert_eq!(volume.metadata().label.as_deref(), Some("MKFS"));
    assert_eq!(volume.metadata().guid, Some(*b"0123456789abcdef"));
    assert_eq!(volume.boot().partition_offset, 2048);
}

#[test]
fn labels_follow_sections_7_3_and_7_7_3() {
    for label in ["", "ÉTÉ", "eleven-char", "😀😀😀😀😀"] {
        let device = Memory::new(4 * MIB);
        let options = FormatOptions {
            label: Some(label.into()),
            ..options(512)
        };
        let layout = mkfs::format(&device, &options).unwrap();
        let volume = interface::open_device(Image(device.snapshot()), 0).unwrap();
        assert_eq!(volume.metadata().label.as_deref(), Some(label));
        assert_accepted(Image(device.snapshot()), &layout);
    }
    let device = Memory::new(4 * MIB);
    let options = FormatOptions {
        label: None,
        ..options(512)
    };
    let layout = mkfs::format(&device, &options).unwrap();
    assert_eq!(mkfs::root_entries(&layout)[0], 0x81);
    let volume = interface::open_device(Image(device.snapshot()), 0).unwrap();
    assert_eq!(volume.metadata().label, None);
}

#[test]
fn refused_layouts_write_nothing() {
    let refusals: [(&str, FormatOptions, u64); 12] = [
        (
            "sector",
            FormatOptions {
                bytes_per_sector: Some(513),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "sector",
            FormatOptions {
                bytes_per_sector: Some(8192),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "cluster",
            FormatOptions {
                cluster_size: Some(3000),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "cluster",
            FormatOptions {
                cluster_size: Some(64 << 20),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "cluster",
            FormatOptions {
                cluster_size: Some(2048),
                ..options(4096)
            },
            8 * MIB,
        ),
        (
            "alignment",
            FormatOptions {
                alignment: Some(100),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "FATs",
            FormatOptions {
                number_of_fats: 3,
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "label",
            FormatOptions {
                label: Some("twelve chars".into()),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "label",
            FormatOptions {
                label: Some("a*b".into()),
                ..options(512)
            },
            8 * MIB,
        ),
        (
            "label",
            FormatOptions {
                label: Some("tab\t".into()),
                ..options(512)
            },
            8 * MIB,
        ),
        ("1 MiB", options(512), MIB - 512),
        (
            "too small",
            FormatOptions {
                cluster_size: Some(512 << 10),
                ..options(512)
            },
            MIB,
        ),
    ];
    for (reason, options, length) in refusals {
        let device = Memory::new(length);
        let error = mkfs::format(&device, &options).unwrap_err();
        assert!(
            matches!(error, FormatError::Refused(_)),
            "{reason}: {error}"
        );
        assert!(device.events.lock().unwrap().is_empty(), "{reason}");
    }
    let device = Memory::new(8 * MIB);
    let larger = FormatOptions {
        size_bytes: Some(9 * MIB),
        ..options(512)
    };
    assert!(matches!(
        mkfs::format(&device, &larger),
        Err(FormatError::Refused(_))
    ));
    let mut read_only = Memory::new(8 * MIB);
    read_only.writable = false;
    assert!(matches!(
        mkfs::format(&read_only, &options(512)),
        Err(FormatError::Refused(_))
    ));
    assert!(device.events.lock().unwrap().is_empty());
}

#[test]
fn a_size_formats_only_the_start_of_the_device() {
    let device = Memory::new(16 * MIB);
    let options = FormatOptions {
        size_bytes: Some(8 * MIB),
        ..options(512)
    };
    let layout = mkfs::format(&device, &options).unwrap();
    assert_eq!(layout.volume_bytes(), 8 * MIB);
    let bytes = device.snapshot();
    assert!(bytes[8 * MIB as usize..].iter().all(|&byte| byte == 0));
    assert_accepted(Image(bytes[..8 * MIB as usize].to_vec()), &layout);
}

/// Sparse files of several GiB, through the same file device the CLI and C
/// ABI use.
#[test]
fn sparse_files_of_several_gib_are_accepted_and_check_clean() {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tmp/mkfs-unit-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for (sector, length, fats) in [(512, 6 * GIB, 1), (4096, 6 * GIB + 4096, 2)] {
        let path = directory.join(format!("sparse-{sector}.img"));
        std::fs::File::create(&path)
            .unwrap()
            .set_len(length)
            .unwrap();
        let device = FileDevice::open_rw(&path).unwrap();
        let options = FormatOptions {
            number_of_fats: fats,
            ..options(sector)
        };
        let layout = mkfs::format(&device, &options).unwrap();
        drop(device);
        assert_eq!(layout.cluster_size(), 32 << 10);
        assert_accepted(FileDevice::open(&path).unwrap(), &layout);
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::remove_dir(&directory).unwrap();
}
