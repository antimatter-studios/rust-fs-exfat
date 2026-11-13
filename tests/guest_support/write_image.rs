//! Fresh, independently formatted images for the write-direction oracles.
//! Tools and loop devices stay in the guest; only a detached image crosses
//! the share. Other write tests can reuse the producer and the checker.

use super::{guest, guest_path, quote, repo, stage_image};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct WriteImage {
    directory: PathBuf,
    pub path: PathBuf,
}

impl WriteImage {
    pub fn new(tier: &str, sector: u32, cluster: u32) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let scratch = repo().join("tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let directory = scratch.join(format!(
            "linux-write-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join(format!("allocation-s{sector}-c{cluster}.img"));
        let script = format!(
            r#"
for tool in mkfs.exfat fsck.exfat losetup mount umount mountpoint modprobe python3 truncate cp mktemp mkdir rm; do
    command -v "$tool" >/dev/null || {{ echo "missing $tool; run 'chore vm:provision'" >&2; exit 1; }}
done
scratch="$(mktemp -d /var/tmp/exfat-write.XXXXXX)"
loop=''
cleanup() {{
    status=$?
    trap - EXIT
    if mountpoint -q "$scratch/mnt"; then
        umount "$scratch/mnt" || exit 1
    fi
    [ -z "$loop" ] || losetup -d "$loop" || exit 1
    rm -rf "$scratch"
    exit "$status"
}}
trap cleanup EXIT
image="$scratch/new.img"
truncate -s 64M "$image"
loop="$(losetup -f --show -b {sector} "$image")"
mkfs.exfat -c {cluster} -L ALLOCATION "$loop" >/dev/null
modprobe exfat || {{ echo "missing exfat module; run 'chore vm:provision'" >&2; exit 1; }}
mkdir "$scratch/mnt"
mount -t exfat "$loop" "$scratch/mnt"
mkdir "$scratch/mnt/keep"
python3 - "$scratch/mnt/keep/sentinel.bin" {cluster} <<'PY'
import os, sys
with open(sys.argv[1], 'wb') as output:
    output.write(bytes(range(256)) * (int(sys.argv[2]) // 256 * 3) + b'preserve this tail')
os.utime(sys.argv[1], (1700000000, 1700000000))
os.utime(os.path.dirname(sys.argv[1]), (1700000000, 1700000000))
PY
umount "$scratch/mnt"
fsck.exfat -n "$loop"
losetup -d "$loop"
loop=''
cp --sparse=always "$image" {destination}
"#,
            destination = quote(&guest_path(&path)),
        );
        guest(
            tier,
            &format!("format and seed s{sector}/c{cluster}"),
            &script,
        )
        .success("fresh write image (run `chore vm:provision` for guest tools)");
        assert!(
            path.is_file(),
            "the guest did not return its detached image"
        );
        Self { directory, path }
    }

    /// Check-only, on a private guest-disk copy of the image the crate wrote.
    pub fn fsck(&self, tier: &str) {
        let script = format!(
            "command -v fsck.exfat >/dev/null || {{ echo \"missing fsck.exfat; run 'chore vm:provision'\" >&2; exit 1; }}\n\
             {stage}fsck.exfat -n \"$image\"\n",
            stage = stage_image(&self.path),
        );
        guest(tier, "fsck.exfat -n after allocation/free", &script)
            .success("fsck.exfat -n on the crate-written image");
    }
}

impl WriteImage {
    /// Where the checkpoint after `kind` is kept: beside the image, named
    /// for the operation, and retained with it when a check fails.
    pub fn checkpoint_path(&self, kind: &str) -> PathBuf {
        let stem = self.path.file_stem().unwrap().to_string_lossy();
        self.directory.join(format!("{stem}-checkpoint-{kind}.img"))
    }

    /// Check-only, one guest call for every checkpoint in `kinds`, each on a
    /// private guest-disk copy. Every checkpoint must be clean; a refused one
    /// is named with fsck.exfat's report and its retained image.
    pub fn fsck_checkpoints(&self, tier: &str, kinds: &[&str]) {
        let mut script = String::from(
            r#"command -v fsck.exfat >/dev/null || { echo "missing fsck.exfat; run 'chore vm:provision'" >&2; exit 1; }
scratch="$(mktemp -d /var/tmp/exfat-oracle.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT
check() {
    cp --sparse=always "$2" "$scratch/image"
    if fsck.exfat -n "$scratch/image" > "$scratch/out" 2>&1; then
        echo "checkpoint clean: $1"
    else
        echo "checkpoint refused: $1 (status $?)"
        cat "$scratch/out"
    fi
    rm -f "$scratch/image"
}
"#,
        );
        for kind in kinds {
            script.push_str(&format!(
                "check {} {}\n",
                quote(kind),
                quote(&guest_path(&self.checkpoint_path(kind)))
            ));
        }
        let name = self
            .path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let out = guest(
            tier,
            &format!(
                "fsck.exfat -n on {} write-kind checkpoints of {name}",
                kinds.len()
            ),
            &script,
        )
        .success("fsck.exfat -n on the write-kind checkpoints");
        let refused: Vec<String> = kinds
            .iter()
            .filter(|kind| {
                !out.stdout
                    .lines()
                    .any(|line| line == format!("checkpoint clean: {kind}"))
            })
            .map(|kind| format!("{kind} ({})", self.checkpoint_path(kind).display()))
            .collect();
        assert!(
            refused.is_empty(),
            "fsck.exfat -n refuses the checkpoint after: {}\n{}{}",
            refused.join(", "),
            out.stdout,
            out.stderr
        );
        println!(
            "[{tier} vm] fsck.exfat -n accepted every checkpoint of {name}: {}",
            kinds.join(" ")
        );
    }
}

impl Drop for WriteImage {
    fn drop(&mut self) {
        // Preserve failed runs for diagnosis; remove only this test's files.
        if !std::thread::panicking() {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }
}
