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

impl Drop for Attachment {
    fn drop(&mut self) {
        let mut output = Command::new("/usr/bin/hdiutil")
            .args(["detach", &self.0])
            .output()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("Resource busy")
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(200));
            output = Command::new("/usr/bin/hdiutil")
                .args(["detach", "-force", &self.0])
                .output()
                .unwrap();
        }
        assert!(output.status.success(), "{:?}", output);
        std::fs::write(self.1.join("attachment-state"), "detached").unwrap();
    }
}

#[test]
fn macos_formats_and_checks_a_volume_above_the_old_audit_cap() {
    std::env::var_os("EXFAT_ORACLE_DIR")
        .expect("run chore test:oracle on macOS for the scale formatter/checker oracle");
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
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS");
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
fn macos_agrees_on_a_directory_with_more_than_a_million_entries() {
    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS");
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

    std::env::var_os("EXFAT_ORACLE_DIR").expect("run chore test:oracle on macOS");
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
