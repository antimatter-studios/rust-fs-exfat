use fs_exfat::fixtures::{special, Builder, BITMAP_CLUSTER, FAT_OFFSET, ROOT_CLUSTER, SECTOR};
use fs_exfat::primitives::{
    bitmap_location, checked_bitmap_bit, checked_fat_entry, inspect_structures, read_structures,
    StructureCopy, FAT_END,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn captures() -> Value {
    serde_json::from_str(include_str!("../structures-captured.json")).unwrap()
}

fn image() -> Vec<u8> {
    let mut builder = Builder::new();
    builder.fat(ROOT_CLUSTER, FAT_END);
    builder.allocate(BITMAP_CLUSTER);
    builder.allocate(ROOT_CLUSTER);
    builder.directory(ROOT_CLUSTER, &[special(0x81, BITMAP_CLUSTER, 25)]);
    builder.image
}

fn digest(bytes: &[u8]) -> Value {
    json!({"length": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))})
}

fn summarize(copy: &StructureCopy) -> Value {
    let raw = json!({
        "main": digest(&copy.main), "backup": digest(&copy.backup),
        "fat": digest(&copy.fat), "root": digest(&copy.root), "bitmap": digest(&copy.bitmap),
    });
    match inspect_structures(copy) {
        Err(error) => json!({"raw": raw, "error": error.to_string()}),
        Ok(geometry) => json!({
            "raw": raw, "boot": geometry.boot, "backupBoot": geometry.backup_boot,
            "bootCheck": geometry.boot_check, "rootClusters": geometry.root_clusters,
            "fat": digest(&geometry.fat.iter().flat_map(|entry| entry.to_le_bytes()).collect::<Vec<_>>()),
            "requiredBitmapLength": u64::from(geometry.boot.cluster_count).div_ceil(8),
        }),
    }
}

fn load(image: &[u8], fail: Option<usize>) -> Value {
    let mut reads = Vec::new();
    let loaded = read_structures(|offset, output| {
        reads.push((offset, output.len()));
        if fail == Some(reads.len()) {
            return Err(std::io::Error::other("capture read failure"));
        }
        let start = offset as usize;
        output.copy_from_slice(&image[start..start + output.len()]);
        Ok(())
    });
    let result = match loaded {
        Ok(copy) => summarize(&copy),
        Err(error) => json!({"error": error.to_string()}),
    };
    json!({"result": result, "reads": reads})
}

#[test]
fn structure_bytes_geometry_and_read_order_match_captured_outputs() {
    assert_eq!(load(&image(), None), captures()["structures"]["normal"]);
}

#[test]
fn damaged_and_incomplete_structures_keep_captured_permissive_results() {
    for (name, offset, bytes) in [
        ("damaged_trailer", 510, vec![0, 0]),
        ("damaged_backup_name", 12 * SECTOR + 3, vec![0; 8]),
        ("wrong_name", 3, vec![0; 8]),
        ("unsupported_sector", 108, vec![12]),
        (
            "missing_bitmap",
            Builder::cluster_position(ROOT_CLUSTER),
            vec![0],
        ),
        (
            "short_bitmap",
            Builder::cluster_position(ROOT_CLUSTER) + 24,
            24u64.to_le_bytes().to_vec(),
        ),
        (
            "oversized_bitmap",
            Builder::cluster_position(ROOT_CLUSTER) + 24,
            33u64.to_le_bytes().to_vec(),
        ),
        (
            "fragmented_root",
            FAT_OFFSET as usize * SECTOR + ROOT_CLUSTER as usize * 4,
            5u32.to_le_bytes().to_vec(),
        ),
        ("zero_fat", 84, 0u32.to_le_bytes().to_vec()),
        ("rounded_bitmap", 92, 201u32.to_le_bytes().to_vec()),
        ("invalid_root", 96, 0u32.to_le_bytes().to_vec()),
    ] {
        let mut image = image();
        image[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert_eq!(load(&image, None), captures()["structures"][name], "{name}");
    }
}

#[test]
fn each_read_failure_keeps_its_captured_context_and_stops_reading() {
    for fail in 1..=5 {
        assert_eq!(
            load(&image(), Some(fail)),
            captures()["structures"][format!("read_failure_{fail}")]
        );
    }
}

fn copy() -> StructureCopy {
    let image = image();
    read_structures(|offset, output| {
        let start = offset as usize;
        output.copy_from_slice(&image[start..start + output.len()]);
        Ok(())
    })
    .unwrap()
}

#[test]
fn saved_copy_root_length_validation_keeps_the_captured_error() {
    let mut copy = copy();
    copy.root.pop();
    assert_eq!(
        inspect_structures(&copy).unwrap_err().to_string(),
        captures()["rootMismatch"]
    );
}

#[test]
fn bitmap_location_reports_declared_and_required_lengths_without_clamping() {
    let mut copy = copy();
    let geometry = inspect_structures(&copy).unwrap();
    copy.root[24..32].copy_from_slice(&33u64.to_le_bytes());
    let location = bitmap_location(&geometry.boot, &copy.root).unwrap();
    assert_eq!(
        (
            location.first_cluster,
            location.length,
            location.required_length
        ),
        (2, 33, 25)
    );
    copy.root[24..32].copy_from_slice(&24u64.to_le_bytes());
    assert_eq!(
        bitmap_location(&geometry.boot, &copy.root)
            .unwrap_err()
            .to_string(),
        captures()["structures"]["short_bitmap"]["result"]["error"]
    );
}

#[test]
fn checked_fat_reads_keep_reserved_words_partial_words_and_out_of_range_results() {
    let fat = [
        0xf8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x12, 0x34, 0x56,
    ];
    for case in captures()["fat"].as_array().unwrap() {
        assert_eq!(
            json!(checked_fat_entry(&fat, case[0].as_u64().unwrap() as usize)),
            case[1]
        );
    }
    assert_eq!(json!(checked_fat_entry(&[], 0)), captures()["emptyFat"]);
    assert_eq!(checked_fat_entry(&fat, usize::MAX), None);
}

#[test]
fn checked_bitmap_reads_keep_captured_false_results_outside_the_snapshot() {
    for case in captures()["bitmap"].as_array().unwrap() {
        assert_eq!(
            json!(checked_bitmap_bit(
                &[0x81, 0x01],
                case[0].as_u64().unwrap() as u32
            )),
            case[1]
        );
    }
    assert_eq!(json!(checked_bitmap_bit(&[], 2)), captures()["emptyBitmap"]);
    assert!(!checked_bitmap_bit(&[0xff], 0));
    assert!(!checked_bitmap_bit(&[0xff], 1));
}

#[test]
fn saved_geometry_matches_independent_captures_including_partial_checksum_sectors() {
    let captures: Value =
        serde_json::from_str(include_str!("../structures-edge-captured.json")).unwrap();
    for name in [
        "minimum_checksum_bytes",
        "main_short",
        "backup_short",
        "wrong_main",
        "wrong_backup",
        "backup_unusable_geometry",
        "unsupported_saved",
    ] {
        let mut copy = copy();
        match name {
            "minimum_checksum_bytes" => {
                copy.main.truncate(SECTOR * 11 + 4);
                copy.backup.truncate(SECTOR * 11 + 4);
            }
            "main_short" => copy.main.truncate(SECTOR - 1),
            "backup_short" => copy.backup.truncate(SECTOR - 1),
            "wrong_main" => copy.main[3] = 0,
            "wrong_backup" => copy.backup[3] = 0,
            "backup_unusable_geometry" => {
                copy.backup[108] = 255;
                copy.backup[109] = 255;
            }
            "unsupported_saved" => copy.main[108] = 12,
            _ => unreachable!(),
        }
        let result = match inspect_structures(&copy) {
            Ok(geometry) => {
                json!({"boot": geometry.boot, "backupBoot": geometry.backup_boot, "bootCheck": geometry.boot_check, "rootClusters": geometry.root_clusters})
            }
            Err(error) => json!({"error": error.to_string()}),
        };
        assert_eq!(result, captures[name], "{name}");
    }
}

#[test]
fn unrepresentable_geometry_and_truncated_checksum_words_return_diagnostics() {
    for (offset, bytes) in [
        (108, vec![255]),
        (109, vec![255]),
        (92, u32::MAX.to_le_bytes().to_vec()),
        (109, vec![54]),
        (109, vec![55]),
    ] {
        let mut copy = copy();
        copy.main[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert!(inspect_structures(&copy).is_err());
        let mut image = image();
        image[offset..offset + bytes.len()].copy_from_slice(&bytes);
        assert_eq!(
            load(&image, None)["result"]["error"],
            "structure size or offset is not representable"
        );
    }
    let mut copy = copy();
    copy.main.truncate(SECTOR * 11 + 3);
    assert_eq!(
        inspect_structures(&copy).unwrap_err().to_string(),
        "structure size or offset is not representable"
    );
}

#[test]
fn overflowing_root_extents_and_reserved_bitmap_locations_return_diagnostics() {
    let mut copy = copy();
    copy.main[109] = 54;
    copy.fat[ROOT_CLUSTER as usize * 4..ROOT_CLUSTER as usize * 4 + 4]
        .copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(
        inspect_structures(&copy).unwrap_err().to_string(),
        "structure size or offset is not representable"
    );
    let mut bytes = image();
    bytes[109] = 54;
    let root_fat = FAT_OFFSET as usize * SECTOR + ROOT_CLUSTER as usize * 4;
    bytes[root_fat..root_fat + 4].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(
        load(&bytes, None)["result"]["error"],
        "structure size or offset is not representable"
    );
    let mut bytes = image();
    let bitmap_cluster = Builder::cluster_position(ROOT_CLUSTER) + 20;
    bytes[bitmap_cluster..bitmap_cluster + 4].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        load(&bytes, None)["result"]["error"],
        "structure size or offset is not representable"
    );
    assert_eq!(checked_fat_entry(&[], usize::MAX / 4), None);
}

#[test]
fn structured_diagnostics_keep_the_underlying_read_and_boot_errors_available() {
    use fs_exfat::primitives::StructureError;

    let diagnostic = StructureError::Read {
        operation: "reading FAT",
        source: std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
    };
    if let StructureError::Read { source, .. } = &diagnostic {
        assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
    } else {
        panic!("read error type was lost");
    }
    assert_eq!(
        StructureError::Boot {
            region: "main",
            source: fs_exfat::Error::Bounds
        }
        .to_string(),
        "main boot sector: read outside device or arithmetic overflow"
    );
}
