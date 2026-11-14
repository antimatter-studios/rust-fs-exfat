use super::image_support::{self as image, Image, Snapshot, CHUNK};
use std::io::{Read, Seek, SeekFrom, Write};

#[path = "../write_support/cuts.rs"]
mod cuts;

#[test]
fn snapshots_preserve_holes_and_comparisons_name_the_first_byte_and_lengths() {
    let fixture = super::support::Temp::new();
    let path = fixture.directory.join("sparse.img");
    let length = (64 * CHUNK + 19) as u64;
    let file = std::fs::File::create_new(&path).unwrap();
    file.set_len(length).unwrap();
    drop(file);
    let source = Image::open(&path);
    for offset in [0, CHUNK - 1, CHUNK, 64 * CHUNK + 18] {
        source.patch(offset, &[0xa5]);
    }
    let snapshot = Snapshot::new(&path);
    snapshot.assert_unchanged(&path);
    assert_eq!(image::digest(&path), image::digest(&snapshot.path));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert!(std::fs::metadata(&snapshot.path).unwrap().blocks() * 512 < length / 4);
    }
    for offset in [0, CHUNK - 1, CHUNK, 64 * CHUNK + 18] {
        source.patch(offset, &[0x5a]);
        let result = image::compare(
            std::fs::File::open(&snapshot.path).unwrap(),
            std::fs::File::open(&path).unwrap(),
            length,
            length,
        )
        .unwrap_err();
        assert_eq!(
            result.to_string(),
            format!("first differing byte at offset {offset} (lengths {length} and {length})")
        );
        source.patch(offset, &[0xa5]);
    }
    source.patch(CHUNK + 5, &[1]);
    source.patch(CHUNK + 9, &[2]);
    let result =
        std::panic::catch_unwind(|| image::assert_equal_from(&snapshot.path, &path, CHUNK as u64));
    let panic = result.unwrap_err();
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(
        message.contains(&format!("offset {}", CHUNK + 5)),
        "{message}"
    );
    source.patch(CHUNK + 5, &[0]);
    source.patch(CHUNK + 9, &[0]);
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(&[1]).unwrap();
    let error = image::compare(
        std::fs::File::open(&snapshot.path).unwrap(),
        std::fs::File::open(&path).unwrap(),
        length,
        length + 1,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "first differing byte at offset {length} (lengths {length} and {})",
            length + 1
        )
    );
    assert!(image::compare(std::io::empty(), std::io::empty(), 1, 1).is_err());
    let snapshot_path = snapshot.path.clone();
    drop(snapshot);
    assert!(!snapshot_path.exists());
}

#[test]
fn image_digest_streams_and_comparison_handles_short_reads() {
    struct Short<R>(R);
    impl<R: Read> Read for Short<R> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let count = output.len().min(37);
            self.0.read(&mut output[..count])
        }
    }
    let length = CHUNK as u64 + 17;
    image::compare(
        Short(std::io::repeat(0).take(length)),
        Short(std::io::repeat(0).take(length)),
        length,
        length,
    )
    .unwrap();
    image::compare(std::io::empty(), std::io::empty(), 0, 0).unwrap();
    let fixture = super::support::Temp::new();
    let path = fixture.directory.join("digest.bin");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        image::digest(&path),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(length).unwrap();
    assert_eq!(
        image::digest(&path),
        "f2c67c81372d31a24e3fe5ac0c10685e97d8d6b671ace65d75fd243d3741f59e"
    );
}

#[test]
fn crash_cut_images_keep_only_changed_sectors_and_round_trip_their_deltas() {
    use fs_core::{BlockDevice, BlockRead};
    let fixture = super::support::Temp::new();
    let original = Snapshot::new(&fixture.image);
    let base = cuts::Image::open(&fixture.image);
    let device = cuts::CutDevice::new(base.clone(), 512, 1);
    assert!(device.write_at(512, &[0xa5; 1024]).is_err());
    assert_eq!(device.written(), 1);
    assert_eq!(device.refused_at(), Some(1024));
    let changed = device.snapshot();
    let mut bytes = [0; 1024];
    changed.read_at(512, &mut bytes).unwrap();
    assert_eq!(&bytes[..512], &[0xa5; 512]);
    assert_eq!(&bytes[512..], Image::open(&fixture.image).read(1024..1536));
    let deltas = cuts::delta(&base, &changed, 512);
    assert_eq!(deltas, [(512, vec![0xa5; 512])]);
    let mut rebuilt = base.clone();
    cuts::apply(&mut rebuilt, &deltas);
    assert!(cuts::delta(&rebuilt, &changed, 512).is_empty());
    assert_eq!(cuts::delta(&base, &changed, 512), deltas);
    let target = fixture.directory.join("rebuilt.img");
    rebuilt.store(&target);
    assert_eq!(cuts::delta(&cuts::Image::open(&target), &changed, 512), []);
    original.assert_unchanged(&fixture.image);
}
