//! Producer for the larger-block write oracle (#159). `scripts/test-oracle.sh`
//! copies the volume macOS formatted and populated with 512-byte sectors,
//! attaches the copy read-write with 4096-byte blocks and without mounting,
//! and hands this test the whole-image node from that attach receipt. The
//! writer changes the volume through the matching raw node, which refuses
//! any transfer smaller than its block, and records the tree this crate's
//! reader then sees. The script checks the result with `fsck_exfat -n` and
//! compares the record with what the read-only mounted macOS driver lists.

mod listing;

use fs_core::block::{BlockDevice, BlockRead};
use fs_core::file_device::FileDevice;
use fs_exfat::interface;
use fs_exfat::write;
use listing::listing;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// The raw node's block, as `-blocksize 4096` attaches it.
const BLOCK: usize = 4096;
/// The volume's own sector, as `newfs_exfat -S 512` formatted it.
const SECTOR: u64 = 512;

unsafe extern "C" {
    fn fsync(descriptor: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// The raw node through `FileDevice`, flushed with `fsync`.
/// `FileDevice::flush` asks for F_FULLFSYNC, which a raw character node
/// answers with ENOTTY (rust-fs-core#440); until that is fixed, this device
/// flushes the node the way it supports. Reads, writes and the size are
/// `FileDevice`'s own.
struct RawNode {
    device: FileDevice,
    node: File,
}

impl BlockRead for RawNode {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> fs_core::error::Result<()> {
        self.device.read_at(offset, buffer)
    }

    fn size_bytes(&self) -> u64 {
        self.device.size_bytes()
    }
}

impl BlockDevice for RawNode {
    fn write_at(&self, offset: u64, buffer: &[u8]) -> fs_core::error::Result<()> {
        self.device.write_at(offset, buffer)
    }

    fn flush(&self) -> fs_core::error::Result<()> {
        // SAFETY: the descriptor belongs to `self.node`, open for this call.
        if unsafe { fsync(self.node.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.device.is_writable()
    }
}

/// The raw node of the whole-image node the attach receipt named.
fn raw_node() -> (PathBuf, PathBuf) {
    let directory = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS"),
    );
    let block = PathBuf::from(
        std::env::var("EXFAT_ORACLE_DEVICE").expect("the oracle requires its live attach receipt"),
    );
    let name = block.file_name().unwrap().to_str().unwrap();
    let number = name.strip_prefix("disk").unwrap();
    assert!(!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()));
    (directory, block.with_file_name(format!("rdisk{number}")))
}

/// The node really has larger blocks than the volume's sectors: it refuses
/// a sector-sized transfer, so every write below had to be widened.
fn refuses_sector_transfers(raw: &Path) {
    let node = File::open(raw).unwrap();
    let mut sector = [0; SECTOR as usize];
    let refused = node.read_at(&mut sector, 0).unwrap_err();
    assert_eq!(refused.raw_os_error(), Some(22), "EINVAL: {refused}");
    let mut block = [0; BLOCK];
    assert_eq!(node.read_at(&mut block, 0).unwrap(), BLOCK);
}

#[test]
fn the_writer_changes_a_512_byte_sector_volume_through_a_4096_byte_raw_node() {
    let (directory, raw) = raw_node();
    refuses_sector_transfers(&raw);
    let device = RawNode {
        device: FileDevice::open_rw(&raw).unwrap(),
        node: OpenOptions::new()
            .read(true)
            .write(true)
            .open(&raw)
            .unwrap(),
    };
    let mut volume = write::open_device(device, 0).unwrap();
    assert_eq!(volume.reader().boot().bytes_per_sector(), SECTOR);
    // Lengths and offsets that are neither sector nor block multiples, in
    // files macOS made and files this crate makes, so each write covers part
    // of a block the device must have read first.
    volume.mkdir("/Raw").unwrap();
    volume.mkdir("/Raw/Nested").unwrap();
    volume
        .write_file("/Raw/odd.bin", &[0x5a; 4096 * 3 + 513])
        .unwrap();
    volume
        .write_file("/Raw/Nested/small.txt", b"written through rdisk\n")
        .unwrap();
    volume.pwrite("/ASCII.txt", 3, b"CHANGED").unwrap();
    volume.pwrite("/contiguous.bin", 600_001, b"tail").unwrap();
    volume.truncate("/item-100.txt", 7).unwrap();
    volume.rename("/MixedCase.txt", "/Raw/moved.txt").unwrap();
    volume.unlink("/item-007.txt").unwrap();
    volume.unmount().unwrap();
    let tree = listing(&interface::open_path(&raw, 0).unwrap());
    assert!(tree.contains("\tRaw/odd.bin\n"), "{tree}");
    std::fs::write(directory.join("raw-written-expected.txt"), tree).unwrap();
}
