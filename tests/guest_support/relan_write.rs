//! relan's write interface, used only as a guest-side black-box oracle.

use crate::guest_support::{guest, guest_path, quote, repo, stage_image};
use crate::walk_support::RELAN_PIN;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct ExpectedEntry {
    pub directory: bool,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub attributes: u16,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub entries: BTreeMap<String, ExpectedEntry>,
    pub label: String,
    pub modified_ms: i64,
    pub accessed_ms: i64,
}

pub struct Checkpoint {
    pub phase: &'static str,
    pub path: PathBuf,
    pub expected: Model,
}

/// Keep every detached phase inside the WriteImage's own cleanup directory.
/// The byte/hash model is independent of both filesystem readers.
pub fn produce(image: &Path, sector: u32, cluster: u32) -> Vec<Checkpoint> {
    let phases = ["create", "truncate", "rename", "empty-label"];
    let mut script = format!(
        r#"{stage}
loop=''
cleanup() {{
    status=$?
    trap - EXIT
    if mountpoint -q "$scratch/mnt"; then umount "$scratch/mnt" || exit 1; fi
    [ -z "$loop" ] || losetup -d "$loop" || exit 1
    rm -rf "$scratch"
    exit "$status"
}}
trap cleanup EXIT
[ "$(cat /opt/relan-exfat/relan-exfat.pin)" = {pin} ] || {{ echo "relan pin differs; run 'chore vm:provision'" >&2; exit 1; }}
for tool in mount.exfat-fuse exfatattrib exfatlabel exfatfsck; do
    [ -x "/opt/relan-exfat/sbin/$tool" ] || {{ echo "missing relan $tool; run 'chore vm:provision'" >&2; exit 1; }}
done
mkdir "$scratch/mnt"
modprobe fuse
loop="$(losetup -f --show -b {sector} "$image")"
export TZ=UTC
"#,
        stage = stage_image(image),
        pin = quote(RELAN_PIN),
    );
    let paths: Vec<_> = phases
        .iter()
        .map(|phase| image.with_file_name(format!("relan-{phase}.img")))
        .collect();
    for (phase, path) in phases.iter().zip(&paths) {
        let model = path.with_extension("json");
        if *phase != "empty-label" {
            script.push_str(&format!(
                "/opt/relan-exfat/sbin/mount.exfat-fuse -o noatime \"$loop\" \"$scratch/mnt\"\n\
                 python3 {producer} \"$scratch/mnt\" {cluster} {phase} {model}\n\
                 umount \"$scratch/mnt\"\n",
                producer = quote(&guest_path(
                    &repo().join("tests/guest_support/relan-produce.py")
                )),
                phase = quote(phase),
                model = quote(&guest_path(&model)),
            ));
        } else {
            script.push_str(&format!(
                "python3 {producer} \"$scratch/mnt\" {cluster} empty-label {model}\n",
                producer = quote(&guest_path(
                    &repo().join("tests/guest_support/relan-produce.py")
                )),
                model = quote(&guest_path(&model)),
            ));
        }
        if *phase == "rename" {
            script.push_str(
                "/opt/relan-exfat/sbin/exfatattrib -r -i -s -a -d \"$loop\" /Relan/CASE.TXT\n\
                 /opt/relan-exfat/sbin/exfatattrib -R -I -S -A -d \"$loop\" /Relan/empty\n\
                 /opt/relan-exfat/sbin/exfatattrib -R -i -s -A -d \"$loop\" '/keep/Moved directory'\n\
                 /opt/relan-exfat/sbin/exfatattrib -R -i -S -a -d \"$loop\" /Relan/replaced.bin\n\
                 /opt/relan-exfat/sbin/exfatlabel \"$loop\" 'RELAN-Δ'\n",
            );
        } else if *phase == "empty-label" {
            script.push_str("/opt/relan-exfat/sbin/exfatlabel \"$loop\" ''\n");
        }
        script.push_str(&format!(
            "fsck.exfat -n \"$loop\"\n\
             /opt/relan-exfat/sbin/exfatfsck -n \"$loop\"\n\
             cp --sparse=always \"$image\" {destination}\n",
            destination = quote(&guest_path(path)),
        ));
    }
    guest(
        "third",
        &format!("relan creates/writes, truncates/removes, renames, sets times/flags/labels s{sector}/c{cluster}; both checkers at each phase"),
        &script,
    )
    .success("relan's write checkpoints (run `chore vm:provision` for tools)");
    phases
        .into_iter()
        .zip(paths)
        .map(|(phase, path)| Checkpoint {
            phase,
            expected: serde_json::from_reader(std::io::BufReader::new(
                std::fs::File::open(path.with_extension("json")).unwrap(),
            ))
            .expect("the producer's complete expected model"),
            path,
        })
        .collect()
}
