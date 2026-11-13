//! Linux formatter oracles, included by the exfatprogs and kernel tiers.
//! Only images created here are staged in the guest, and only the loop
//! device returned for that staged image is mounted or detached.

#![allow(dead_code)]

use crate::guest_support::{guest, guest_path, quote, repo, stage_image};
use fs_core::FileDevice;
use fs_exfat::mkfs::{self, FormatOptions};
use fs_exfat::{check, interface, path::Node};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const CASES: [(u64, u64); 5] = [
    (MIB, 4 << 10),
    (64 * MIB, 4 << 10),
    (300 * MIB, 32 << 10),
    (6 * GIB, 32 << 10),
    (40 * GIB, 128 << 10),
];

struct Image {
    directory: PathBuf,
    path: PathBuf,
    sector: u32,
    size: u64,
    cluster: u64,
}

impl Image {
    fn format(sector: u32, size: u64, cluster: u64) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = repo().join(format!(
            "tmp/linux-mkfs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("formatted.img");
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(size)
            .unwrap();
        let device = FileDevice::open_rw(&path).unwrap();
        let layout = mkfs::format(
            &device,
            &FormatOptions {
                bytes_per_sector: Some(sector),
                label: Some("LINUXMKFS".into()),
                volume_serial: Some(0x1390_ABCD),
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(layout.volume_bytes(), size);
        assert_eq!(layout.cluster_size(), cluster);
        let image = Self {
            directory,
            path,
            sector,
            size,
            cluster,
        };
        image.check(0, 1, &[]);
        image
    }

    fn description(&self) -> String {
        format!(
            "formatter {} bytes, {}-byte sectors, {}-byte clusters",
            self.size, self.sector, self.cluster
        )
    }

    fn check(&self, files: u64, directories: u64, allowed: &[check::Rule]) {
        let volume = interface::open_path(&self.path, 0).unwrap();
        assert_eq!(volume.boot().bytes_per_sector(), u64::from(self.sector));
        assert_eq!(volume.boot().cluster_size(), self.cluster);
        assert_eq!(volume.metadata().label.as_deref(), Some("LINUXMKFS"));
        let report = check::check(&volume).unwrap();
        assert!(report.healthy(), "{}: {report:?}", self.description());
        assert_eq!(report.files, files, "{}", self.description());
        assert_eq!(report.directories, directories, "{}", self.description());
        assert!(report.deviations.complete, "{report:?}");
        let unexpected: Vec<_> = report
            .deviations
            .found
            .iter()
            .filter(|deviation| !allowed.contains(&deviation.rule))
            .collect();
        assert!(unexpected.is_empty(), "{unexpected:?}");
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }
}

pub fn fsck_accepts_formatted_volumes() {
    for sector in [512, 4096] {
        for (size, cluster) in CASES {
            let image = Image::format(sector, size, cluster);
            guest(
                "oracle",
                &format!("{}: fsck.exfat -n", image.description()),
                &format!(
                    "{stage}command -v fsck.exfat >/dev/null || {{ echo 'chore vm:provision provides fsck.exfat' >&2; exit 1; }}\n\
                     fsck.exfat -n \"$image\"\n",
                    stage = stage_image(&image.path),
                ),
            )
            .success("fsck.exfat -n on a newly formatted volume");
        }
    }
}

pub fn kernel_writes_formatted_volumes() {
    let written = [
        (
            "written by Linux.txt",
            b"the Linux exFAT driver wrote this\n".to_vec(),
        ),
        (
            "Folder/nested-λ.bin",
            (0..(2 * (128 << 10) + 513))
                .map(|index| (index % 251) as u8)
                .collect(),
        ),
        ("empty", Vec::new()),
    ];
    for sector in [512, 4096] {
        for (size, cluster) in CASES {
            let image = Image::format(sector, size, cluster);
            let mut copies = String::new();
            for (index, (name, contents)) in written.iter().enumerate() {
                let payload = image.directory.join(format!("payload-{index}"));
                std::fs::write(&payload, contents).unwrap();
                copies.push_str(&format!(
                    "cp {} \"$scratch/mnt/\"{}\n",
                    quote(&guest_path(&payload)),
                    quote(name),
                ));
            }
            let script = format!(
                "{stage}\
                 for tool in fsck.exfat modprobe losetup blockdev mount umount; do\n\
                   command -v \"$tool\" >/dev/null || {{ echo \"chore vm:provision provides $tool\" >&2; exit 1; }}\n\
                 done\n\
                 mkdir \"$scratch/mnt\"\n\
                 loop=''\n\
                 mounted=0\n\
                 cleanup() {{\n\
                   if [ \"$mounted\" = 1 ]; then umount \"$scratch/mnt\" || return; fi\n\
                   if [ -n \"$loop\" ]; then losetup -d \"$loop\" || return; fi\n\
                   rm -rf \"$scratch\"\n\
                 }}\n\
                 trap cleanup EXIT\n\
                 fsck.exfat -n \"$image\"\n\
                 modprobe exfat\n\
                 loop=\"$(losetup --find --show --sector-size {sector} \"$image\")\"\n\
                 [ \"$(blockdev --getss \"$loop\")\" = {sector} ]\n\
                 mount -t exfat -o fmask=0,dmask=0,iocharset=utf8 \"$loop\" \"$scratch/mnt\"\n\
                 mounted=1\n\
                 mkdir \"$scratch/mnt/Folder\"\n\
                 {copies}\
                 umount \"$scratch/mnt\"\n\
                 mounted=0\n\
                 losetup -d \"$loop\"\n\
                 loop=''\n\
                 fsck.exfat -n \"$image\"\n\
                 cp --sparse=always \"$image\" {destination}\n",
                stage = stage_image(&image.path),
                destination = quote(&guest_path(&image.path)),
            );
            guest(
                "kernel",
                &format!("{}: fsck, mount, write, unmount, fsck", image.description()),
                &script,
            )
            .success("the Linux kernel writing a newly formatted volume");
            image.check(3, 2, &[check::Rule::PercentInUse]);
            let volume = interface::open_path(&image.path, 0).unwrap();
            let Node::Entry(directory) = volume.lookup("Folder").unwrap() else {
                panic!("Folder is not a directory entry");
            };
            assert!(directory.is_directory());
            for (name, contents) in &written {
                let Node::Entry(entry) = volume.lookup(name).unwrap() else {
                    panic!("{name} is not a file entry");
                };
                let file = volume.open_file(&entry).unwrap();
                assert_eq!(file.len(), contents.len() as u64, "{name}");
                let mut actual = vec![0; contents.len()];
                let mut offset = 0;
                while offset < actual.len() {
                    let count = file.read_at(offset as u64, &mut actual[offset..]).unwrap();
                    assert_ne!(count, 0, "{name}: unexpected EOF at {offset}");
                    offset += count;
                }
                assert!(
                    actual == *contents,
                    "{}: {name}: contents differ",
                    image.description()
                );
            }
        }
    }
}
