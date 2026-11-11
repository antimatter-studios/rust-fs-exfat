#[path = "../scale_support/mod.rs"]
mod support;

use super::support as base;
use std::os::unix::fs::FileExt;
use std::process::{Command, Stdio};
use support::{clean, Image, OLD_CAP};

struct Attachment(String, std::path::PathBuf);

impl Attachment {
    fn new(image: &Image, readonly: bool) -> Self {
        Self::attach(image, readonly, None)
    }

    fn attach(image: &Image, readonly: bool, mount: Option<&std::path::Path>) -> Self {
        let mut command = Command::new("/usr/bin/hdiutil");
        command.args(["attach", "-nobrowse", "-plist"]);
        if let Some(mount) = mount {
            command.arg("-mountpoint").arg(mount);
        } else {
            command.arg("-nomount");
        }
        if readonly {
            command.arg("-readonly");
        }
        let output = command.arg(&image.path).output().unwrap();
        assert!(output.status.success(), "{:?}", output);
        std::fs::write(image.directory.join("attachment-state"), "attached").unwrap();
        std::fs::write(image.directory.join("attach.plist"), &output.stdout).unwrap();
        let mut parser = Command::new("/usr/bin/python3")
            .args(["-c", "import plistlib,re,sys; p=plistlib.loads(sys.stdin.buffer.read()); d=[e['dev-entry'] for e in p['system-entities'] if re.fullmatch(r'/dev/disk[0-9]+',e.get('dev-entry',''))]; assert len(d)==1; print(d[0])"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut parser.stdin.take().unwrap(), &output.stdout).unwrap();
        let parsed = parser.wait_with_output().unwrap();
        assert!(
            parsed.status.success(),
            "attachment receipt: {:?}",
            output.stdout
        );
        Self(
            String::from_utf8(parsed.stdout).unwrap().trim().to_owned(),
            image.directory.clone(),
        )
    }
}

fn native_check(image: &Image, healthy: bool) {
    let attachment = Attachment::new(image, true);
    let checked = Command::new("/sbin/fsck_exfat")
        .args(["-n", &attachment.0])
        .output()
        .unwrap();
    assert_eq!(checked.status.success(), healthy, "{:?}", checked);
    drop(attachment);
}

/// How long a detach may stay busy before the image is reported left behind.
/// The platform has held a freshly checked million-entry image for longer than
/// the 20 seconds this once allowed (#109).
pub(super) const DETACH_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

impl Drop for Attachment {
    fn drop(&mut self) {
        if let Err(message) = detach(
            std::path::Path::new("/usr/bin/hdiutil"),
            &self.0,
            &self.1,
            DETACH_WINDOW,
        ) {
            // A second panic while a failed test unwinds would abort and lose
            // the message, so it is printed instead.
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

/// Detaches `device` with `program`, retrying with `-force` while it reports
/// the device busy and `window` has not passed, and records the outcome in the
/// `attachment-state` beside the receipt in `directory`.
pub(super) fn detach(
    program: &std::path::Path,
    device: &str,
    directory: &std::path::Path,
    window: std::time::Duration,
) -> Result<(), String> {
    let mut output = Command::new(program)
        .args(["detach", device])
        .output()
        .unwrap();
    let deadline = std::time::Instant::now() + window;
    while !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("Resource busy")
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(200));
        output = Command::new(program)
            .args(["detach", "-force", device])
            .output()
            .unwrap();
    }
    if !output.status.success() {
        return Err(format!(
            "scale oracle: {device} is still attached after {} s of retries ({}: {}). \
             Its image and receipt are retained in {}; release it with: {} detach -force {device}",
            window.as_secs_f64(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            directory.display(),
            program.display(),
        ));
    }
    std::fs::write(directory.join("attachment-state"), "detached").unwrap();
    Ok(())
}

/// A directory holding a stand-in for `hdiutil detach` that answers busy for
/// its first `busy` calls, then succeeds. It touches no device.
fn busy_detacher(busy: u32) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "tmp/detach-stub-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("attachment-state"), "attached").unwrap();
    let program = directory.join("detach-stub");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             count=$(cat \"$0.calls\" 2>/dev/null || echo 0)\n\
             echo $((count + 1)) > \"$0.calls\"\n\
             [ \"$count\" -ge {busy} ] && exit 0\n\
             echo 'hdiutil: couldn'\\''t eject \"disk-stub\" - Resource busy' >&2\n\
             exit 16\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    (directory, program)
}

#[test]
fn a_detach_that_clears_within_its_window_records_the_image_detached() {
    let (directory, program) = busy_detacher(2);
    detach(
        &program,
        "/dev/disk-stub",
        &directory,
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    let state = std::fs::read_to_string(directory.join("attachment-state")).unwrap();
    assert_eq!(state, "detached");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_detach_that_stays_busy_names_the_command_that_releases_the_image() {
    let (directory, program) = busy_detacher(u32::MAX);
    let message = detach(
        &program,
        "/dev/disk-stub",
        &directory,
        std::time::Duration::from_millis(500),
    )
    .unwrap_err();
    let state = std::fs::read_to_string(directory.join("attachment-state")).unwrap();
    assert_eq!(state, "attached");
    let command = format!("{} detach -force /dev/disk-stub", program.display());
    assert!(message.contains(&command), "{message}");
    assert!(
        message.contains(&directory.display().to_string()),
        "{message}"
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn macos_formats_and_checks_a_volume_above_the_old_audit_cap() {
    std::env::var_os("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle-macos on macOS for the scale formatter/checker oracle");
    let image = Image::sparse(OLD_CAP + 65536, 0);
    let attachment = Attachment::new(&image, false);
    let formatted = Command::new("/sbin/newfs_exfat")
        .args(["-R", "-S", "512", "-b", "512", "-v", "SCALE"])
        .arg(&attachment.0)
        .output()
        .unwrap();
    assert!(formatted.status.success(), "{:?}", formatted);
    drop(attachment);
    let attachment = Attachment::new(&image, true);
    let checked = Command::new("/sbin/fsck_exfat")
        .args(["-n", &attachment.0])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    drop(attachment);
    let report = clean(image.check());
    assert_eq!(report["unreferenced_clusters"], 0);
    let boot = fs_exfat::boot::BootRegions::read(&fs_exfat::device::Reader::new(
        fs_core::FileDevice::open(&image.path).unwrap(),
    ))
    .unwrap();
    assert!(boot.strict().unwrap().cluster_count > OLD_CAP);
}

#[test]
fn macos_agrees_on_high_cluster_cross_links_and_unreferenced_allocations() {
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    let image = Image::sparse(OLD_CAP + 65536, 0);
    image.high_file(0, OLD_CAP + 17);
    native_check(&image, true);
    clean(image.check());
    image.high_file(1, OLD_CAP + 17);
    native_check(&image, false);
    assert_eq!(image.check().status.code(), Some(4));
    image
        .file
        .write_all_at(&[0; 96], image.heap + 64 + 96)
        .unwrap();
    image.allocate(OLD_CAP + 29);
    native_check(&image, false);
    let output = image.check();
    assert_eq!(output.status.code(), Some(4));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["unreferenced_clusters"], 1);
    assert_eq!(report["healthy"], false);
}

#[test]
fn macos_agrees_that_an_allocated_cluster_the_fat_marks_bad_is_not_lost() {
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    let image = Image::sparse(65536, 0);
    let cluster = image.allocated as u32 + 7;
    image
        .file
        .write_all_at(
            &0xffff_fff7u32.to_le_bytes(),
            24 * 512 + u64::from(cluster) * 4,
        )
        .unwrap();
    image.allocate(cluster);
    native_check(&image, true);
    let report = clean(image.check());
    assert_eq!(report["bad_clusters"], 1);
    assert_eq!(report["unreferenced_clusters"], 0);
}

#[test]
fn macos_agrees_that_a_vendor_allocations_cluster_is_not_lost() {
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    let image = Image::sparse(65536, 0);
    let first = image.allocated as u32 + 7;
    let owned = first + 1;
    let mut set = base::file_set("vendor", 0x20, first, 512, true);
    set[1] += 1;
    let mut vendor = [0u8; 32];
    vendor[0] = 0xe1;
    vendor[1] = 1;
    vendor[2..18].fill(0x5a);
    base::put32(&mut vendor, 20, owned);
    base::put64(&mut vendor, 24, 512);
    set.extend_from_slice(&vendor);
    base::seal_set(&mut set);
    image.allocate(first);
    image.allocate(owned);
    image
        .file
        .write_all_at(&u32::MAX.to_le_bytes(), 24 * 512 + u64::from(owned) * 4)
        .unwrap();
    image.file.write_all_at(&set, image.heap + 64).unwrap();
    native_check(&image, true);
    let report = clean(image.check());
    assert_eq!(report["unreferenced_clusters"], 0);
}

#[test]
fn macos_agrees_on_a_directory_with_more_than_a_million_entries() {
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    let image = Image::sparse(OLD_CAP + 65536, OLD_CAP + 1);
    native_check(&image, true);
    let report = clean(image.check());
    assert_eq!(report["files"], u64::from(OLD_CAP) + 1);
}

#[test]
fn large_files_match_fsck_and_streamed_read_only_macos_driver_hashes() {
    use fs_exfat::volume::Volume;
    use sha2::{Digest, Sha256};
    use std::io::Read;

    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    for contiguous in [false, true] {
        let image = Image::sparse(OLD_CAP + 4096, 0);
        image.long_file(OLD_CAP + 1, contiguous);
        let length = u64::from(OLD_CAP + 1) * 512;
        let start = image.heap + image.allocated * 512;
        for (offset, byte) in [
            (0, 17),
            (length / 2 + 13, 53),
            (length - 513, 91),
            (length - 1, 137),
        ] {
            image.file.write_all_at(&[byte], start + offset).unwrap();
        }
        native_check(&image, true);
        let mount = image.directory.join("readonly-mount");
        std::fs::create_dir(&mount).unwrap();
        let attachment = Attachment::attach(&image, true, Some(&mount));
        let mut native = std::fs::File::open(mount.join("long")).unwrap();
        assert_eq!(native.metadata().unwrap().len(), length);
        let mut native_hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = native.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            native_hash.update(&buffer[..count]);
        }
        drop(native);
        drop(attachment);
        let volume = Volume::open(fs_core::FileDevice::open(&image.path).unwrap()).unwrap();
        let entry = volume.root_directory().unwrap().remove(0);
        let file = volume.open_file(&entry).unwrap();
        assert_eq!(file.len(), length);
        let mut observed_hash = Sha256::new();
        let mut offset = 0;
        loop {
            let count = file.read_at(offset, &mut buffer).unwrap();
            if count == 0 {
                break;
            }
            observed_hash.update(&buffer[..count]);
            offset += count as u64;
        }
        assert_eq!(offset, length);
        assert_eq!(observed_hash.finalize(), native_hash.finalize());
    }
}

/// Salvage's cross-link listing names the clusters macOS `fsck_exfat -n`
/// refuses for being reached twice: two records claiming one cluster, and
/// two FAT entries naming one cluster.
#[test]
fn macos_refuses_the_cross_links_salvage_lists() {
    use fs_exfat::salvage::{CrossLink, ExfatSalvage, Extent, SalvageAdapter};
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle-macos on macOS");
    let listing = |image: &Image| {
        let adapter = ExfatSalvage::probe(image);
        let mut candidates = Vec::new();
        adapter
            .find_candidates(image, Extent::new(image.heap, 512), &mut candidates)
            .unwrap();
        let mut found: Vec<CrossLink> = Vec::new();
        let report = adapter.cross_links(image, &candidates, &mut found).unwrap();
        assert_eq!(report.unclassified + report.past_source, 0);
        let records: Vec<_> = candidates
            .iter()
            .map(|found| found.location.offset)
            .collect();
        (found, records)
    };
    let fat = |image: &Image, cluster: u32, value: u32| {
        image
            .file
            .write_all_at(&value.to_le_bytes(), 24 * 512 + u64::from(cluster) * 4)
            .unwrap();
    };

    // Two contiguous records claiming one cluster.
    let image = Image::sparse(65536, 0);
    let shared = image.allocated as u32 + 7;
    image.high_file(0, shared);
    native_check(&image, true);
    assert!(listing(&image).0.is_empty());
    image.high_file(1, shared);
    native_check(&image, false);
    let (found, records) = listing(&image);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].cluster, shared);
    assert!(found[0].links.is_empty());
    assert_eq!(found[0].candidates, records);
    drop(image);

    // Two FAT chains that meet: first, first + 1 and other, first + 1.
    let image = Image::sparse(65536, 0);
    let first = image.allocated as u32 + 7;
    let other = first + 13;
    for (slot, start) in [(0, first), (1, other)] {
        image
            .file
            .write_all_at(
                &base::file_set(&format!("chain{slot}"), 0x20, start, 1024, false),
                image.heap + 64 + slot * 96,
            )
            .unwrap();
    }
    for cluster in [first, first + 1, other, other + 1] {
        image.allocate(cluster);
    }
    fat(&image, first, first + 1);
    fat(&image, first + 1, u32::MAX);
    fat(&image, other, other + 1);
    fat(&image, other + 1, u32::MAX);
    native_check(&image, true);
    assert!(listing(&image).0.is_empty());
    // other + 1 is freed, so only the meeting chains are wrong.
    fat(&image, other, first + 1);
    fat(&image, other + 1, 0);
    let index = u64::from(other + 1 - 2);
    let mut byte = [0];
    image
        .file
        .read_exact_at(&mut byte, image.bitmap + index / 8)
        .unwrap();
    byte[0] &= !(1 << (index % 8));
    image
        .file
        .write_all_at(&byte, image.bitmap + index / 8)
        .unwrap();
    native_check(&image, false);
    let (found, records) = listing(&image);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].cluster, first + 1);
    let links: Vec<_> = found[0]
        .links
        .iter()
        .map(|link| (link.from, link.head))
        .collect();
    assert_eq!(links, [(first, first), (other, other)]);
    assert_eq!(found[0].candidates, records);
}
