#[path = "../scale_support/mod.rs"]
mod support;

use crate::support as base;
use fs_exfat::volume::{ReadLimits, Volume};
use fs_exfat::Error;
use std::os::unix::fs::FileExt;
use support::{Image, OLD_CAP};

#[test]
fn default_file_reads_and_fat_walks_exceed_a_million_clusters_without_truncation() {
    for contiguous in [false, true] {
        let image = Image::sparse(OLD_CAP + 4096, 0);
        image.long_file(OLD_CAP + 1, contiguous);
        let length = u64::from(OLD_CAP + 1) * 512;
        let first = image.allocated + 2;
        let markers = [
            (0, 17),
            (length / 2 + 13, 53),
            (length - 513, 91),
            (length - 1, 137),
        ];
        for (offset, byte) in markers {
            image
                .file
                .write_all_at(&[byte], image.heap + (first - 2) * 512 + offset)
                .unwrap();
        }
        let volume = Volume::open(&image).unwrap();
        let entry = volume.root_directory().unwrap().remove(0);
        let file = volume.open_file(&entry).unwrap();
        assert_eq!(file.len(), length);
        let mut output = [0xff; 64 * 1024];
        let mut offset = 0;
        while offset < length {
            let count = file.read_at(offset, &mut output).unwrap();
            assert_eq!(count, (length - offset).min(output.len() as u64) as usize);
            let mut expected = [0; 64 * 1024];
            for (position, byte) in markers {
                if (offset..offset + count as u64).contains(&position) {
                    expected[(position - offset) as usize] = byte;
                }
            }
            assert_eq!(&output[..count], &expected[..count], "offset {offset}");
            offset += count as u64;
        }
        assert_eq!(file.read_at(length, &mut output).unwrap(), 0);
        if !contiguous {
            let chain = volume.cluster_chain(first as u32).unwrap();
            assert_eq!(chain.len(), OLD_CAP as usize + 1);
            assert_eq!(chain.last(), Some(&((first + u64::from(OLD_CAP)) as u32)));
        }
    }
}

#[test]
fn contiguous_files_need_no_per_cluster_index_budget() {
    let image = Image::sparse(OLD_CAP + 4096, 0);
    image.long_file(OLD_CAP + 1, true);
    let limits = ReadLimits {
        max_metadata_bytes: (OLD_CAP as usize + 4096).div_ceil(8),
        ..ReadLimits::default()
    };
    let volume = Volume::open_with_limits(&image, limits).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    let file = volume.open_file(&entry).unwrap();
    assert_eq!(
        file.read_at(u64::from(OLD_CAP) * 512, &mut [0xff; 512])
            .unwrap(),
        512
    );
    image.long_file(OLD_CAP + 1, false);
    let entry = volume.root_directory().unwrap();
    assert!(entry[0].stream().no_fat_chain());
    let volume = Volume::open_with_limits(&image, limits).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    assert!(matches!(
        volume.open_file(&entry),
        Err(Error::Unsupported("chain exceeds memory budget"))
    ));
}

#[test]
fn fat_chain_index_budget_is_exact_and_over_budget_is_an_error() {
    let image = Image::sparse(4096, 0);
    image.long_file(2048, false);
    let limits = ReadLimits {
        max_metadata_bytes: 8192,
        ..ReadLimits::default()
    };
    let volume = Volume::open_with_limits(&image, limits).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    assert_eq!(volume.open_file(&entry).unwrap().len(), 2048 * 512);
    assert_eq!(
        volume
            .cluster_chain(entry.stream().first_cluster())
            .unwrap()
            .len(),
        2048
    );
    image.long_file(2049, false);
    let volume = Volume::open_with_limits(&image, limits).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    assert!(matches!(
        volume.open_file(&entry),
        Err(Error::Unsupported("chain exceeds memory budget"))
    ));
    assert!(matches!(
        volume.cluster_chain(entry.stream().first_cluster()),
        Err(Error::Unsupported("chain exceeds memory budget"))
    ));
}

#[test]
fn caller_cluster_caps_still_apply_to_both_stream_layouts() {
    for contiguous in [false, true] {
        let image = Image::sparse(4096, 0);
        image.long_file(17, contiguous);
        let limits = ReadLimits {
            max_chain_clusters: 16,
            ..ReadLimits::default()
        };
        let volume = Volume::open_with_limits(&image, limits).unwrap();
        let entry = volume.root_directory().unwrap().remove(0);
        assert!(matches!(
            volume.open_file(&entry),
            Err(Error::Unsupported("stream exceeds chain read limits"))
        ));
    }
}

#[test]
fn chain_cycles_and_late_io_errors_never_return_partial_chains() {
    let image = Image::sparse(4096, 0);
    image.long_file(3000, false);
    let first = image.allocated as u32 + 2;
    image
        .file
        .write_all_at(
            &(first + 37).to_le_bytes(),
            24 * 512 + u64::from(first + 2999) * 4,
        )
        .unwrap();
    let volume = Volume::open(&image).unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    assert!(volume.open_file(&entry).is_err());
    assert!(volume.cluster_chain(first).is_err());
    image.long_file(3000, false);
    image
        .file
        .write_all_at(&0u32.to_le_bytes(), 24 * 512 + u64::from(first + 2999) * 4)
        .unwrap();
    assert!(matches!(
        volume.cluster_chain(first),
        Err(Error::Invalid("free or bad cluster in FAT chain"))
    ));
}

#[test]
fn allocation_validation_reaches_beyond_the_old_cluster_cap() {
    for contiguous in [false, true] {
        let image = Image::sparse(OLD_CAP + 4096, 0);
        image.long_file(OLD_CAP + 1, contiguous);
        let last = image.allocated + 2 + u64::from(OLD_CAP);
        let position = image.bitmap + (last - 2) / 8;
        let mut byte = [0];
        image.file.read_exact_at(&mut byte, position).unwrap();
        byte[0] &= !(1 << ((last - 2) % 8));
        image.file.write_all_at(&byte, position).unwrap();
        let volume = Volume::open(&image).unwrap();
        assert_eq!(
            volume.limits().max_chain_clusters,
            volume.boot().cluster_count as usize
        );
        let entry = volume.root_directory().unwrap().remove(0);
        assert!(matches!(volume.open_file(&entry), Err(Error::Invalid(_))));
    }
}

struct FailedChainRead<'image> {
    image: &'image Image,
    fail_at: u64,
}

impl fs_core::BlockRead for FailedChainRead<'_> {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        if offset == self.fail_at {
            return Err(fs_core::Error::Custom(
                "injected late chain I/O failure".into(),
            ));
        }
        self.image.read_at(offset, output)
    }

    fn size_bytes(&self) -> u64 {
        self.image.size_bytes()
    }
}

#[test]
fn late_fat_io_failures_are_not_returned_as_successful_partial_chains() {
    let image = Image::sparse(4096, 0);
    image.long_file(3000, false);
    let first = image.allocated as u32 + 2;
    let fail_at = (24 * 512 + u64::from(first + 2999) * 4) / 512 * 512;
    let volume = Volume::open(FailedChainRead {
        image: &image,
        fail_at,
    })
    .unwrap();
    let entry = volume.root_directory().unwrap().remove(0);
    let failure = volume.cluster_chain(first).unwrap_err();
    assert!(matches!(failure, Error::Device(_)));
    assert!(failure
        .to_string()
        .contains("injected late chain I/O failure"));
    assert!(matches!(volume.open_file(&entry), Err(Error::Device(_))));
}
