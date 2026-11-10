use crate::support;

use fs_exfat::directory;
use fs_exfat::volume::{ReadLimits, Volume};
use fs_exfat::Error;
use support::{file_set, metadata_volume, put32, Bytes};

#[test]
fn strict_chain_policy_preserves_budget_and_damage_diagnostics() {
    let mut data = metadata_volume();
    data[33 * 512] |= 0x18;
    put32(&mut data, 24 * 512 + 5 * 4, 6);
    put32(&mut data, 24 * 512 + 6 * 4, 5);
    for maximum in [1, 2, 3] {
        let volume = Volume::open_with_limits(
            Bytes(data.clone()),
            ReadLimits {
                max_metadata_bytes: 512,
                max_chain_clusters: maximum,
            },
        )
        .unwrap();
        assert_eq!(volume.limits().max_chain_clusters, maximum);
        assert_eq!(volume.limits().max_metadata_bytes, 512);
        assert_eq!(volume.root_bytes(), &data[32 * 512..33 * 512]);
        for first in [0, 1, 102, u32::MAX] {
            assert!(matches!(
                volume.cluster_chain(first),
                Err(Error::Invalid("cluster outside heap"))
            ));
        }
        let error = volume.cluster_chain(5).unwrap_err();
        if maximum < 3 {
            assert!(matches!(
                error,
                Error::Unsupported("chain exceeds read limits")
            ));
        } else {
            assert!(matches!(error, Error::Invalid("FAT chain cycle")));
        }
        let entry = directory::parse(
            &file_set("directory", 0x10, 5, 1024, true),
            volume.boot(),
            volume.upcase(),
            false,
        )
        .unwrap();
        let error = volume.read_directory(&entry[0]).unwrap_err();
        if maximum == 1 {
            assert!(matches!(
                error,
                Error::Unsupported("stream exceeds chain read limits")
            ));
        } else {
            assert!(matches!(
                error,
                Error::Unsupported("metadata exceeds read limits")
            ));
        }
        let entry = directory::parse(
            &file_set("file", 0x20, 5, 512, true),
            volume.boot(),
            volume.upcase(),
            false,
        )
        .unwrap();
        assert!(matches!(
            volume.read_directory(&entry[0]),
            Err(Error::Invalid("entry is not a directory"))
        ));
    }
    put32(&mut data, 24 * 512 + 3 * 4, 5);
    put32(&mut data, 24 * 512 + 5 * 4, u32::MAX);
    assert!(matches!(
        Volume::open(Bytes(data)),
        Err(Error::Invalid("system stream chain length"))
    ));
}
