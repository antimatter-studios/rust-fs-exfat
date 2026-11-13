//! The guest half of the block-cut matrix (#188): exfatprogs judges every
//! partial image, and repairs a private copy of each one it refuses.
//!
//! Every cut image crosses the share as the sectors in which it differs
//! from the cut before it (the first from the freshly formatted image), in
//! one file, so a few hundred images cost a few megabytes. The guest
//! rebuilds them in order on its own disk and, for each one:
//!
//! - `fsck.exfat -n` on the image as the cut left it;
//! - if that refuses it, `fsck.exfat -y` on a copy, then `fsck.exfat -n`
//!   on the repaired copy, and the sectors the repair changed, returned so
//!   the host can rebuild the repaired image and read it with the strict
//!   reader.
//!
//! The repair runs on the copy only; nothing here repairs anything this
//! crate writes in production.

use super::{guest, guest_path, quote};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

/// Whole sectors by byte offset: what differs from the image before.
pub type Sectors = Vec<(u64, Vec<u8>)>;

/// exfatprogs' verdict on one cut.
pub struct Verdict {
    /// `fsck.exfat -n` on the cut image.
    pub check: i32,
    /// `fsck.exfat -y` on a copy and `fsck.exfat -n` after it, with the
    /// sectors the repair changed; only for a refused cut.
    pub repair: Option<(i32, i32, Sectors)>,
    /// What the checker printed for a refused cut, in order.
    pub report: String,
}

const SCRIPT: &str = r#"
import struct, subprocess, sys
base, deltas, out, scratch = sys.argv[1:5]
work, copy = scratch + '/work.img', scratch + '/repair.img'
subprocess.run(['cp', '--sparse=always', base, work], check=True)

def fsck(mode, image):
    done = subprocess.run(['fsck.exfat', mode, image], stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return done.returncode, done.stdout.decode('utf-8', 'replace')

def changed(sector):
    found = []
    with open(work, 'rb') as a, open(copy, 'rb') as b:
        offset = 0
        while True:
            left, right = a.read(1 << 20), b.read(1 << 20)
            if not left and not right:
                return found
            if left != right:
                for at in range(0, max(len(left), len(right)), sector):
                    if left[at:at + sector] != right[at:at + sector]:
                        found.append((offset + at, right[at:at + sector]))
            offset += len(left)

source = open(deltas, 'rb')
magic, sector, count = struct.unpack('<8sII', source.read(16))
assert magic == b'EXCUTS01', magic
verdicts = open(out + '/verdicts.txt', 'w')
repairs = open(out + '/repairs.bin', 'wb')
report = open(out + '/report.txt', 'w')
refused = 0
with open(work, 'r+b') as image:
    for _ in range(count):
        cut, sectors = struct.unpack('<QI', source.read(12))
        for _ in range(sectors):
            (offset,) = struct.unpack('<Q', source.read(8))
            image.seek(offset)
            image.write(source.read(sector))
        image.flush()
        check, said = fsck('-n', work)
        if check == 0:
            verdicts.write('%d %d\n' % (cut, check))
            continue
        refused += 1
        subprocess.run(['cp', '--sparse=always', work, copy], check=True)
        repair, repaired = fsck('-y', copy)
        after, recheck = fsck('-n', copy)
        fixes = changed(sector)
        repairs.write(struct.pack('<QI', cut, len(fixes)))
        for offset, data in fixes:
            repairs.write(struct.pack('<Q', offset) + data)
        verdicts.write('%d %d %d %d\n' % (cut, check, repair, after))
        report.write('== cut %d\n-- fsck.exfat -n\n%s-- fsck.exfat -y\n%s-- fsck.exfat -n after\n%s'
                     % (cut, said, repaired, recheck))
print('judged %d cuts; fsck.exfat -n refused %d and repaired copies of them' % (count, refused))
"#;

/// Write every cut's sectors to `directory/deltas.bin`, have exfatprogs
/// judge each rebuilt image in one guest call, and return the verdicts by
/// cut. `base` is the freshly formatted image the first delta applies to.
pub fn judge(
    tier: &str,
    what: &str,
    base: &Path,
    directory: &Path,
    sector: u32,
    deltas: &[(u64, Sectors)],
) -> BTreeMap<u64, Verdict> {
    let file = directory.join("deltas.bin");
    let mut output = std::io::BufWriter::new(std::fs::File::create_new(&file).unwrap());
    output.write_all(b"EXCUTS01").unwrap();
    output.write_all(&sector.to_le_bytes()).unwrap();
    output
        .write_all(&(deltas.len() as u32).to_le_bytes())
        .unwrap();
    for (cut, sectors) in deltas {
        output.write_all(&cut.to_le_bytes()).unwrap();
        output
            .write_all(&(sectors.len() as u32).to_le_bytes())
            .unwrap();
        for (offset, bytes) in sectors {
            assert_eq!(bytes.len(), sector as usize);
            output.write_all(&offset.to_le_bytes()).unwrap();
            output.write_all(bytes).unwrap();
        }
    }
    output.into_inner().unwrap().sync_all().unwrap();

    let script = format!(
        r#"for tool in fsck.exfat python3 cp mktemp; do
    command -v "$tool" >/dev/null || {{ echo "missing $tool; run 'chore vm:provision'" >&2; exit 1; }}
done
scratch="$(mktemp -d /var/tmp/exfat-cuts.XXXXXX)"
trap 'rm -rf "$scratch"' EXIT
python3 - {base} {deltas} {out} "$scratch" <<'PY'
{SCRIPT}
PY
"#,
        base = quote(&guest_path(base)),
        deltas = quote(&guest_path(&file)),
        out = quote(&guest_path(directory)),
    );
    let out = guest(tier, what, &script).success("fsck.exfat on the block-cut images");
    print!("{}", out.stdout);

    let mut reports = BTreeMap::new();
    let report = std::fs::read_to_string(directory.join("report.txt")).unwrap();
    for section in report.split("== cut ").skip(1) {
        let (cut, text) = section.split_once('\n').unwrap();
        reports.insert(cut.parse::<u64>().unwrap(), text.to_owned());
    }
    let mut repairs = BTreeMap::new();
    let mut source = std::fs::read(directory.join("repairs.bin"))
        .unwrap()
        .into_iter();
    let mut take = |count: usize| -> Vec<u8> { source.by_ref().take(count).collect() };
    loop {
        let head = take(12);
        if head.is_empty() {
            break;
        }
        let cut = u64::from_le_bytes(head[..8].try_into().unwrap());
        let count = u32::from_le_bytes(head[8..].try_into().unwrap());
        let sectors = (0..count)
            .map(|_| {
                let offset = u64::from_le_bytes(take(8).try_into().unwrap());
                (offset, take(sector as usize))
            })
            .collect::<Vec<_>>();
        repairs.insert(cut, sectors);
    }

    let mut verdicts = BTreeMap::new();
    let mut text = String::new();
    std::fs::File::open(directory.join("verdicts.txt"))
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    for line in text.lines() {
        let fields: Vec<i64> = line
            .split(' ')
            .map(|field| field.parse().unwrap())
            .collect();
        let cut = fields[0] as u64;
        let verdict = match fields[..] {
            [_, check] => Verdict {
                check: check as i32,
                repair: None,
                report: String::new(),
            },
            [_, check, repair, after] => Verdict {
                check: check as i32,
                repair: Some((
                    repair as i32,
                    after as i32,
                    repairs.remove(&cut).expect("the repair's sectors"),
                )),
                report: reports.remove(&cut).unwrap_or_default(),
            },
            _ => panic!("unreadable verdict line {line:?}"),
        };
        verdicts.insert(cut, verdict);
    }
    assert_eq!(
        verdicts.len(),
        deltas.len(),
        "the guest judged {} of {} cuts",
        verdicts.len(),
        deltas.len()
    );
    verdicts
}
