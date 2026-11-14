use fs_core::BlockRead;
use fs_exfat::device::Reader;
use std::sync::{Arc, Mutex};

#[path = "support/image.rs"]
mod image_support;
#[path = "interface_support/mod.rs"]
mod support;

#[path = "support/image_tests.rs"]
mod image_tests;

type Calls = Arc<Mutex<Vec<(u64, usize)>>>;

struct ReadOnly {
    calls: Calls,
}

impl BlockRead for ReadOnly {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        assert_eq!(offset % 512, 0);
        assert_eq!(output.len() % 512, 0);
        self.calls.lock().unwrap().push((offset, output.len()));
        for (index, byte) in output.iter_mut().enumerate() {
            *byte = (offset as usize + index) as u8;
        }
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        2048
    }
}

fn reader() -> (Reader<ReadOnly>, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Reader::new(ReadOnly {
            calls: calls.clone(),
        }),
        calls,
    )
}

#[test]
fn accepts_a_device_implementing_only_block_read() {
    let (reader, _) = reader();
    assert_eq!(reader.size_bytes(), 2048);
}

#[test]
fn unaligned_read_uses_only_aligned_device_reads() {
    let (reader, calls) = reader();
    let mut output = [0; 517];
    reader.read_at(509, &mut output).unwrap();
    assert_eq!(*calls.lock().unwrap(), [(0, 512), (512, 512), (1024, 512)]);
    assert_eq!(output[0], 253);
    assert_eq!(output[516], 1);
}

#[test]
fn out_of_bounds_and_overflow_do_not_reach_device() {
    let (reader, calls) = reader();
    assert!(reader.read_at(2048, &mut [0]).is_err());
    assert!(reader.read_at(u64::MAX, &mut [0]).is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn empty_read_at_end_does_not_reach_device() {
    let (reader, calls) = reader();
    reader.read_at(2048, &mut []).unwrap();
    assert!(calls.lock().unwrap().is_empty());
}

struct RawDevice {
    bytes: Vec<u8>,
    sector: usize,
    calls: Calls,
}

impl RawDevice {
    fn new(bytes: Vec<u8>, sector: usize) -> Self {
        Self {
            bytes,
            sector,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl BlockRead for RawDevice {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.calls.lock().unwrap().push((offset, output.len()));
        assert!(offset + output.len() as u64 <= self.size_bytes());
        if !offset.is_multiple_of(self.sector as u64) || !output.len().is_multiple_of(self.sector) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput).into());
        }
        output.copy_from_slice(&self.bytes[offset as usize..offset as usize + output.len()]);
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
}

#[test]
fn raw_sector_reads_cross_boundaries_and_keep_bounds() {
    for sector in [512, 1024, 2048, 4096, 8192] {
        let bytes: Vec<_> = (0..sector * 3).map(|index| (index % 251) as u8).collect();
        let device = RawDevice::new(bytes.clone(), sector);
        let calls = device.calls.clone();
        let reader = Reader::new(device);
        let mut output = vec![0; sector + 7];
        reader.read_at((sector - 3) as u64, &mut output).unwrap();
        assert_eq!(output, bytes[sector - 3..2 * sector + 4]);
        let before = calls.lock().unwrap().len();
        let mut tail = [0; 7];
        reader.read_at((bytes.len() - 7) as u64, &mut tail).unwrap();
        assert_eq!(tail, bytes[bytes.len() - 7..]);
        assert!(calls.lock().unwrap()[before..]
            .iter()
            .all(|(offset, length)| {
                offset.is_multiple_of(sector as u64) && length.is_multiple_of(sector)
            }));
        let before = calls.lock().unwrap().len();
        reader.read_at(bytes.len() as u64, &mut []).unwrap();
        assert!(reader.read_at(bytes.len() as u64, &mut [0]).is_err());
        assert!(reader.read_at(u64::MAX, &mut [0]).is_err());
        assert_eq!(calls.lock().unwrap().len(), before);
    }
}

#[test]
fn raw_device_volumes_open_and_read_with_512_byte_volume_sectors() {
    for sector in [512, 4096, 8192] {
        let device = RawDevice::new(support::image(), sector);
        let volume = fs_exfat::volume::Volume::open(device).unwrap();
        let entry = volume.lookup("Example.txt").unwrap();
        let file = volume.open_file(entry.entry().unwrap()).unwrap();
        let mut output = vec![0; file.len() as usize];
        assert_eq!(file.read_at(0, &mut output).unwrap(), output.len());
        assert_eq!(output, support::expected());
        assert_eq!(volume.boot().bytes_per_sector(), 512);
    }
}

#[test]
fn raw_device_alignment_is_applied_before_volume_offsets() {
    for sector in [512, 4096, 8192] {
        for offset in [0, 512, 4096] {
            let image = support::image();
            let mut bytes = vec![0xa5; (image.len() + offset).div_ceil(sector) * sector];
            bytes[offset..offset + image.len()].copy_from_slice(&image);
            let volume =
                fs_exfat::interface::open_device(RawDevice::new(bytes, sector), offset as u64)
                    .unwrap();
            let entry = volume.lookup("Example.txt").unwrap();
            let file = volume.open_file(entry.entry().unwrap()).unwrap();
            let mut output = vec![0; file.len() as usize];
            assert_eq!(file.read_at(0, &mut output).unwrap(), output.len());
            assert_eq!(output, support::expected());
        }
    }
}

#[test]
fn raw_device_rejects_unaligned_volume_offsets_without_io() {
    let device = RawDevice::new(support::image(), 4096);
    let calls = device.calls.clone();
    assert!(fs_exfat::interface::open_device(device, 513).is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn alignment_probes_remain_within_incomplete_devices() {
    for length in [512, 1536, 4096] {
        let device = RawDevice::new(vec![0; length], 8192);
        let reader = Reader::new(device);
        let error = reader.read_at((length - 1) as u64, &mut [0]).unwrap_err();
        assert!(
            matches!(error, fs_exfat::Error::Device(fs_core::Error::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput)
        );
    }
}

#[test]
fn non_alignment_errors_are_not_retried() {
    struct FailedDevice(Calls, std::io::ErrorKind);

    impl BlockRead for FailedDevice {
        fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
            self.0.lock().unwrap().push((offset, output.len()));
            Err(std::io::Error::from(self.1).into())
        }

        fn size_bytes(&self) -> u64 {
            8192
        }
    }

    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let reader = Reader::new(FailedDevice(calls.clone(), kind));
        let error = reader.read_at(0, &mut [0]).unwrap_err();
        assert!(
            matches!(error, fs_exfat::Error::Device(fs_core::Error::Io(error)) if error.kind() == kind)
        );
        assert_eq!(*calls.lock().unwrap(), [(0, 512)]);
    }
}
