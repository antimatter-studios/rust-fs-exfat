use fs_exfat::fixtures::*;
use fs_exfat::primitives::FAT_END;
use sha2::{Digest, Sha256};

fn captured(name: &str, bytes: &[u8]) {
    let record = include_str!("fixtures-captured.sha256")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|fields| fields[2] == name)
        .expect("missing independently captured fixture output");
    assert_eq!(bytes.len(), record[1].parse::<usize>().unwrap(), "{name}");
    assert_eq!(format!("{:x}", Sha256::digest(bytes)), record[0], "{name}");
}

#[test]
fn empty_matches_captured_bytes() {
    captured("empty", &Builder::new().image);
}

#[test]
fn main_boot_matches_captured_bytes() {
    captured("main_boot", &Builder::new().image[..SECTOR * 12]);
}

#[test]
fn backup_boot_matches_captured_bytes() {
    captured(
        "backup_boot",
        &Builder::new().image[SECTOR * 12..SECTOR * 24],
    );
}

#[test]
fn live_matches_captured_bytes() {
    captured("live", &entry_set("keep.mp3", false, 10, 8192, true, true));
}

#[test]
fn deleted_matches_captured_bytes() {
    captured(
        "deleted",
        &entry_set("gone", true, 6, CLUSTER as u64, true, false),
    );
}

#[test]
fn unicode_matches_captured_bytes() {
    captured(
        "unicode",
        &entry_set("long-Grüße-😀-filename.txt", false, 30, 12345, false, true),
    );
}

#[test]
fn empty_name_matches_captured_bytes() {
    captured("empty_name", &entry_set("", false, 0, 0, true, false));
}

#[test]
fn modified_matches_captured_bytes() {
    captured(
        "modified",
        &modified_on(
            entry_set("Store", true, 8, CLUSTER as u64, true, true),
            2026,
            11,
            4,
        ),
    );
}

#[test]
fn created_matches_captured_bytes() {
    captured(
        "created",
        &created_on(
            entry_set("old.txt", false, 131, 100, true, false),
            2020,
            1,
            1,
        ),
    );
}

#[test]
fn bitmap_entry_matches_captured_bytes() {
    captured(
        "bitmap_entry",
        &special(0x81, BITMAP_CLUSTER, u64::from(CLUSTER_COUNT).div_ceil(8)),
    );
}

#[test]
fn upcase_placeholder_matches_captured_bytes() {
    captured("upcase_placeholder", &special(0x82, 4, 100));
}

#[test]
fn noise_matches_captured_bytes() {
    captured("noise", &noise(u32::MAX, CLUSTER));
}

#[test]
fn build_volume_matches_captured_bytes() {
    captured("build_volume", &build_volume());
}

#[test]
fn reuse_image_matches_captured_bytes() {
    captured("reuse_image", &reuse_image());
}

#[test]
fn volume_macos_has_visited_matches_captured_bytes() {
    captured("volume_macos_has_visited", &volume_macos_has_visited());
}

#[test]
fn build_jigsaw_volume_matches_captured_bytes() {
    captured("build_jigsaw_volume", &build_jigsaw_volume());
}

#[test]
fn placement_matches_captured_bytes() {
    captured("placement", &{
        let mut builder = Builder::new();
        for cluster in [2, 3, 9, 10, 201] {
            builder.allocate(cluster);
        }
        builder.fat(3, FAT_END);
        builder.fat(10, 12);
        builder.fat(12, FAT_END);
        builder.directory(
            ROOT_CLUSTER,
            &[
                special(0x81, BITMAP_CLUSTER, 25),
                special(0x82, 4, 100),
                entry_set("fragment.bin", false, 10, 5000, false, true),
                entry_set("deleted", false, 50, 16, true, false),
            ],
        );
        builder.put(10, 17, &noise(7, CLUSTER + 3));
        builder.image
    });
}

#[test]
fn resealed_matches_captured_bytes() {
    captured("resealed", &{
        let mut set = entry_set("changed", false, 12, 8000, false, false);
        set[40..48].copy_from_slice(&123u64.to_le_bytes());
        seal_entry_set(&mut set);
        set
    });
}
