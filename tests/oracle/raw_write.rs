//! Producer for the larger-block write oracle (#159). `scripts/test-oracle.sh`
//! copies the volume macOS formatted and populated with 512-byte sectors,
//! attaches the copy read-write with 4096-byte blocks and without mounting,
//! and hands this test the whole-image node from that attach receipt. The
//! writer opens the matching raw node with `write::open_path`, changes the
//! volume through it although the node refuses any transfer smaller than
//! its block, and records the tree this crate's reader then sees. The script checks the result with `fsck_exfat -n` and
//! compares the record with what the read-only mounted macOS driver lists.

mod listing;

use fs_exfat::interface;
use fs_exfat::write;
use listing::listing;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// The raw node's block, as `-blocksize 4096` attaches it.
const BLOCK: usize = 4096;
/// The volume's own sector, as `newfs_exfat -S 512` formatted it.
const SECTOR: u64 = 512;

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
    // The crate's own path opener, as a consumer would use it: its
    // `FileDevice` flushes the raw node, which refuses F_FULLFSYNC with
    // ENOTTY, through rust-fs-core 0.4.5's `fsync` fallback.
    let mut volume = write::open_path(&raw, 0).unwrap();
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
    // The unmount flushed through the raw node and cleared VolumeDirty
    // (bit 1 of VolumeFlags, at byte 106 of the main boot sector).
    let mut boot = [0; BLOCK];
    File::open(&raw)
        .unwrap()
        .read_exact_at(&mut boot, 0)
        .unwrap();
    let flags = u16::from_le_bytes([boot[106], boot[107]]);
    assert_eq!(flags & 0x0002, 0, "VolumeDirty left set: {flags:#06x}");
    let tree = listing(&interface::open_path(&raw, 0).unwrap());
    assert!(tree.contains("\tRaw/odd.bin\n"), "{tree}");
    std::fs::write(directory.join("raw-written-expected.txt"), tree).unwrap();
}
