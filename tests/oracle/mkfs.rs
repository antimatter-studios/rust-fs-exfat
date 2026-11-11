//! The formatter against macOS: `fsck_exfat -n`, the exFAT driver, and the
//! volumes `newfs_exfat` lays down at the same size and options.
//!
//! Every image is a sparse file this test creates in its own directory under
//! `tmp/`, and every device node comes from the attach receipt of that image;
//! no other device is listed or touched.

use fs_core::{BlockRead, FileDevice};
use fs_exfat::mkfs::{self, FormatOptions, Layout};
use fs_exfat::{check, interface};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const HDIUTIL: &str = "/usr/bin/hdiutil";
const SERIAL: u32 = 0x1234_ABCD;
const LABEL: &str = "MKFS";

/// Sizes from the smallest volume to several GiB, across both default
/// cluster-size steps below 128 KiB and one above.
const SIZES: [u64; 5] = [MIB, 64 * MIB, 300 * MIB, 6 * GIB, 40 * GIB];

fn require_oracle() {
    std::env::var_os("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle-macos on macOS for the formatter oracle");
}

/// A private directory holding one sparse image file.
struct Image {
    directory: PathBuf,
    path: PathBuf,
    sector: u32,
}

impl Image {
    fn sparse(name: &str, length: u64, sector: u32) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "tmp/mkfs-oracle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{name}.img"));
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length)
            .unwrap();
        Self {
            directory,
            path,
            sector,
        }
    }

    fn format(&self, options: &FormatOptions) -> Layout {
        let device = FileDevice::open_rw(&self.path).unwrap();
        mkfs::format(&device, options).unwrap()
    }

    fn bytes(&self, offset: u64, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        FileDevice::open(&self.path)
            .unwrap()
            .read_at(offset, &mut bytes)
            .unwrap();
        bytes
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }
}

/// The image attached by `hdiutil`, by the one whole-image device its own
/// receipt names; detached on drop.
struct Attachment {
    device: String,
    directory: PathBuf,
}

impl Attachment {
    fn new(image: &Image, readonly: bool, mount: Option<&Path>) -> Self {
        let mut command = Command::new(HDIUTIL);
        command
            .args(["attach", "-nobrowse", "-plist", "-imagekey"])
            .arg("diskimage-class=CRawDiskImage")
            .arg("-blocksize")
            .arg(image.sector.to_string());
        match mount {
            Some(mount) => command.arg("-mountpoint").arg(mount),
            None => command.arg("-nomount"),
        };
        if readonly {
            command.arg("-readonly");
        }
        let output = command.arg(&image.path).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        std::fs::write(image.directory.join("attachment-state"), "attached").unwrap();
        std::fs::write(image.directory.join("attach.plist"), &output.stdout).unwrap();
        let mut parser = Command::new("/usr/bin/python3")
            .args(["-c", "import plistlib,re,sys; p=plistlib.loads(sys.stdin.buffer.read()); d=[e['dev-entry'] for e in p['system-entities'] if re.fullmatch(r'/dev/disk[0-9]+',e.get('dev-entry',''))]; assert len(d)==1; print(d[0])"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut parser.stdin.take().unwrap(), &output.stdout).unwrap();
        let parsed = parser.wait_with_output().unwrap();
        assert!(parsed.status.success(), "attachment receipt: {output:?}");
        Self {
            device: String::from_utf8(parsed.stdout).unwrap().trim().to_owned(),
            directory: image.directory.clone(),
        }
    }

    fn run(&self, program: &str, arguments: &[&str]) -> Output {
        Command::new(program)
            .args(arguments)
            .arg(&self.device)
            .output()
            .unwrap()
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        if let Err(message) = super::scale::detach(
            Path::new(HDIUTIL),
            &self.device,
            &self.directory,
            super::scale::DETACH_WINDOW,
        ) {
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

/// `fsck_exfat -n` finds the volume clean.
fn macos_check(image: &Image) {
    let attachment = Attachment::new(image, true, None);
    let checked = attachment.run("/sbin/fsck_exfat", &["-n"]);
    let report = String::from_utf8_lossy(&checked.stdout);
    assert!(
        checked.status.success() && report.contains("appears to be OK"),
        "{}: {checked:?}",
        image.path.display()
    );
}

fn options(sector: u32) -> FormatOptions {
    FormatOptions {
        bytes_per_sector: Some(sector),
        label: Some(LABEL.into()),
        volume_serial: Some(SERIAL),
        ..FormatOptions::default()
    }
}

/// This crate's check finds the volume healthy, holding `files` when given,
/// and it breaks no rule of the specification apart from those `allowed`.
fn crate_check(image: &Image, files: Option<u64>, allowed: &[check::Rule]) {
    let volume = interface::open_path(&image.path, 0).unwrap();
    let report = check::check(&volume).unwrap();
    assert!(report.healthy(), "{report:?}");
    if let Some(files) = files {
        assert_eq!(report.files, files);
    }
    let unexpected: Vec<_> = report
        .deviations
        .found
        .iter()
        .filter(|deviation| !allowed.contains(&deviation.rule))
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:?}");
    assert!(report.deviations.complete);
}

/// What the driver is given to write, and must read back.
const WRITTEN: [(&str, &[u8]); 2] = [
    (
        "written by macOS.txt",
        b"the macOS exFAT driver wrote this\n",
    ),
    ("Folder/nested.bin", &[0xA5; 70_000]),
];

#[test]
fn macos_checks_mounts_and_writes_formatted_volumes() {
    require_oracle();
    for sector in [512, 4096] {
        for size in SIZES {
            let image = Image::sparse("formatted", size, sector);
            let layout = image.format(&options(sector));
            assert_eq!(layout.volume_bytes(), size);
            crate_check(&image, Some(0), &[]);
            macos_check(&image);

            // The driver mounts it read-write, reports its label and accepts
            // files and a directory.
            let mount = image.directory.join("mount");
            std::fs::create_dir(&mount).unwrap();
            {
                let attachment = Attachment::new(&image, false, Some(&mount));
                let info = attachment.run("/usr/sbin/diskutil", &["info", "-plist"]);
                let info = String::from_utf8_lossy(&info.stdout);
                assert!(info.contains("<string>MKFS</string>"), "{info}");
                assert!(info.contains("<string>exfat</string>"), "{info}");
                std::fs::write(mount.join(".metadata_never_index"), b"").unwrap();
                std::fs::create_dir(mount.join("Folder")).unwrap();
                for (name, contents) in WRITTEN {
                    std::fs::write(mount.join(name), contents).unwrap();
                }
            }
            macos_check(&image);
            // macOS leaves PercentInUse as it was after writing; spec
            // 3.1.18 asks for an update or FFh (see tests/oracle/boot.rs). It
            // also adds .fseventsd and AppleDouble files, a varying number.
            crate_check(&image, None, &[check::Rule::PercentInUse]);
            let volume = interface::open_path(&image.path, 0).unwrap();
            for (name, contents) in WRITTEN {
                let node = volume.lookup(name).unwrap();
                let fs_exfat::path::Node::Entry(entry) = node else {
                    panic!("{name} is the root");
                };
                let file = volume.open_file(&entry).unwrap();
                let mut read = vec![0; contents.len()];
                assert_eq!(file.read_at(0, &mut read).unwrap(), contents.len());
                assert_eq!(
                    read, contents,
                    "{name} at {size} bytes, {sector}-byte sectors"
                );
            }
        }
    }
}

/// Formats an image of `size` bytes with `newfs_exfat`, the same label and
/// serial number, and `extra` arguments.
fn newfs(size: u64, sector: u32, extra: &[&str]) -> Image {
    let image = Image::sparse("newfs", size, sector);
    let attachment = Attachment::new(&image, false, None);
    let sector_text = sector.to_string();
    let serial = format!("0x{SERIAL:08x}");
    let mut arguments = vec!["-R", "-S", &sector_text, "-v", LABEL, "-I", &serial];
    arguments.extend_from_slice(extra);
    // macOS may still hold a freshly attached node while it probes it, and
    // newfs_exfat then answers busy; it is retried for a while, as a busy
    // detach is in scale.rs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut formatted = attachment.run("/sbin/newfs_exfat", &arguments);
    while !formatted.status.success()
        && String::from_utf8_lossy(&formatted.stderr).contains("Resource busy")
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(500));
        formatted = attachment.run("/sbin/newfs_exfat", &arguments);
    }
    assert!(
        formatted.status.success(),
        "newfs_exfat, retried while busy for up to 60 s: {formatted:?}"
    );
    drop(attachment);
    image
}

/// Offsets at which two equally long files differ.
fn differences(first: &Path, second: &Path) -> Vec<u64> {
    use std::io::Read;
    let mut first = std::io::BufReader::new(std::fs::File::open(first).unwrap());
    let mut second = std::io::BufReader::new(std::fs::File::open(second).unwrap());
    let (mut left, mut right) = (vec![0; 1 << 20], vec![0; 1 << 20]);
    let mut found = Vec::new();
    let mut position = 0u64;
    loop {
        let count = first.read(&mut left).unwrap();
        second.read_exact(&mut right[..count]).unwrap();
        if count == 0 {
            return found;
        }
        found.extend(
            (0..count)
                .filter(|&index| left[index] != right[index])
                .map(|index| position + index as u64),
        );
        position += count as u64;
    }
}

/// With newfs_exfat's alignment of 64 KiB, a 64 MiB volume is the same,
/// byte for byte, at both sector sizes. On a two-FAT volume the only
/// difference is FatEntry[3] in each FAT: newfs_exfat leaves the Second
/// Allocation Bitmap's one cluster at 0, where section 4.1.3 requires the
/// last entry of a chain to be FFFFFFFFh, and this formatter writes that.
#[test]
fn newfs_exfat_lays_down_the_same_bytes_at_the_same_alignment() {
    require_oracle();
    for sector in [512, 4096] {
        let options = FormatOptions {
            alignment: Some(64 << 10),
            ..options(sector)
        };
        let ours = Image::sparse("ours", 64 * MIB, sector);
        ours.format(&options);
        let theirs = newfs(64 * MIB, sector, &[]);
        assert_eq!(
            differences(&ours.path, &theirs.path),
            [] as [u64; 0],
            "{sector}"
        );

        let options = FormatOptions {
            number_of_fats: 2,
            ..options
        };
        let ours = Image::sparse("ours-texfat", 64 * MIB, sector);
        let layout = ours.format(&options);
        let theirs = newfs(64 * MIB, sector, &["-n", "2"]);
        let entry = |copy: u64| {
            let fat = u64::from(layout.fat_offset) + copy * u64::from(layout.fat_length);
            let start = fat * u64::from(sector) + 3 * 4;
            start..start + 4
        };
        let expected: Vec<u64> = entry(0).chain(entry(1)).collect();
        assert_eq!(differences(&ours.path, &theirs.path), expected, "{sector}");
        assert_eq!(theirs.bytes(entry(0).start, 4), [0; 4]);
        assert_eq!(ours.bytes(entry(0).start, 4), [0xFF; 4]);
        macos_check(&ours);
        crate_check(&ours, Some(0), &[]);
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// Boot sector bytes that hold FatOffset, FatLength, ClusterHeapOffset and
/// ClusterCount (section 3.1, Table 4).
const ALIGNED_FIELDS: std::ops::Range<usize> = 80..96;

/// At the default alignment, the only boot-sector fields that differ from
/// newfs_exfat's are FatOffset, FatLength, ClusterHeapOffset and
/// ClusterCount: newfs_exfat starts the FAT and heap on boundaries of 64 KiB
/// to 16 MiB that grow with the volume, where this formatter uses the cluster
/// size. The Allocation Bitmap's DataLength and the boot checksum follow from
/// the cluster count. Everything else -- the cluster size, every other boot
/// field, the FAT chains, the bitmap's allocated bits, the root directory's
/// entries and the up-case table -- is the same.
#[test]
fn at_the_default_alignment_only_the_aligned_fields_differ_from_newfs_exfat() {
    require_oracle();
    for sector in [512, 4096] {
        for size in SIZES {
            let ours = Image::sparse("ours", size, sector);
            let layout = ours.format(&options(sector));
            let theirs = newfs(size, sector, &[]);
            let boot = |image: &Image| image.bytes(0, sector as usize);
            let (mine, other) = (boot(&ours), boot(&theirs));
            let context = format!("{size} bytes, {sector}-byte sectors");
            assert_eq!(
                mine[..ALIGNED_FIELDS.start],
                other[..ALIGNED_FIELDS.start],
                "{context}"
            );
            assert_eq!(
                mine[ALIGNED_FIELDS.end..],
                other[ALIGNED_FIELDS.end..],
                "{context}"
            );
            // newfs_exfat's FAT and heap are aligned to at least 64 KiB, ours
            // to the cluster size.
            let field = |bytes: &[u8], offset| u64::from(u32_at(bytes, offset)) * u64::from(sector);
            for offset in [80, 88] {
                assert_eq!(field(&other, offset) % (64 << 10), 0, "{context}");
                assert_eq!(field(&mine, offset) % layout.cluster_size(), 0, "{context}");
            }
            let extended = |image: &Image| image.bytes(u64::from(sector), 10 * sector as usize);
            assert_eq!(extended(&ours), extended(&theirs), "{context}");

            let theirs_volume = interface::open_path(&theirs.path, 0).unwrap();
            let ours_volume = interface::open_path(&ours.path, 0).unwrap();
            let chains = |volume: &fs_exfat::volume::Volume<fs_core::OwnedSlice>| {
                let metadata = volume.metadata();
                let bitmap = metadata.bitmaps[0].unwrap();
                [
                    bitmap.first_cluster,
                    metadata.upcase.first_cluster,
                    volume.boot().root_cluster,
                ]
                .map(|first| volume.cluster_chain(first).unwrap())
            };
            assert_eq!(chains(&ours_volume), chains(&theirs_volume), "{context}");
            let root = |volume: &fs_exfat::volume::Volume<fs_core::OwnedSlice>| {
                let mut root = volume.root_bytes()[..96].to_vec();
                root[56..64].fill(0); // the Allocation Bitmap's DataLength
                root
            };
            assert_eq!(root(&ours_volume), root(&theirs_volume), "{context}");
            let upcase = |volume: &fs_exfat::volume::Volume<fs_core::OwnedSlice>| {
                let stream = volume.metadata().upcase;
                (
                    stream.first_cluster,
                    stream.length,
                    volume.metadata().upcase_checksum,
                )
            };
            assert_eq!(upcase(&ours_volume), upcase(&theirs_volume), "{context}");
            let bits = |volume: &fs_exfat::volume::Volume<fs_core::OwnedSlice>| {
                (2..2 + layout.allocated_clusters() + 8)
                    .map(|cluster| volume.bitmap().allocated(cluster).unwrap())
                    .collect::<Vec<_>>()
            };
            assert_eq!(bits(&ours_volume), bits(&theirs_volume), "{context}");
        }
    }
}
