//! The stable half of the fuzzing setup: replay the corpus, mutate it, and refuse
//! if a decoder panics or hangs, if a decoder has no target, or if a corpus stopped
//! reaching past the decoder's first check.
//!
//! `fuzz/` holds the cargo-fuzz targets. Those are the explorer: how long they run
//! decides what they find, so a fresh discovery would fail whichever unrelated pull
//! request happened to be open, and they cannot be a required check. This suite is
//! the gate: deterministic, on the pinned toolchain, in every pull request, over the
//! same `fuzz/corpus/`. Anything the explorer finds is committed there and replayed
//! here from then on.

#[path = "../fuzz/shared/mod.rs"]
mod shared;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

struct Target {
    name: &'static str,
    decoders: &'static [&'static str],
    run: fn(&[u8]) -> bool,
    /// Mutated cases per seed. Per target, because a case costs what its seed costs.
    rounds: usize,
}

const TARGETS: &[Target] = &[
    Target {
        name: "boot_sector",
        decoders: &[
            "BootSector::parse",
            "BootSector::parse",
            "BootSectorHeader::parse",
            "BootRegion::parse",
        ],
        run: shared::boot_sector,
        rounds: 2000,
    },
    Target {
        name: "fat_entry",
        decoders: &["FatEntry::parse"],
        run: shared::fat_entry,
        rounds: 2000,
    },
    Target {
        name: "allocation_bitmap",
        decoders: &["AllocationBitmap::parse"],
        run: shared::allocation_bitmap,
        rounds: 200,
    },
    Target {
        name: "upcase_table",
        decoders: &["UpcaseTable::parse"],
        run: shared::upcase_table,
        rounds: 200,
    },
    Target {
        name: "root_metadata",
        decoders: &["RootMetadata::parse"],
        run: shared::root_metadata,
        rounds: 400,
    },
    Target {
        name: "directory",
        decoders: &["directory::parse", "EntryType::parse", "RawEntrySet::parse"],
        run: shared::directory,
        rounds: 200,
    },
    Target {
        name: "timestamp",
        decoders: &["Timestamp::decode"],
        run: shared::timestamp,
        rounds: 2000,
    },
    Target {
        name: "timestamps",
        decoders: &["Timestamps::parse"],
        run: shared::timestamps,
        rounds: 2000,
    },
    Target {
        name: "image",
        decoders: &[],
        run: shared::image,
        rounds: 20,
    },
];

/// Fixed, so a failure reproduces from its message alone.
const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// Long enough that a loaded machine is never the reason, short enough that a real
/// hang is reported rather than left to the job's timeout.
const DEADLINE: Duration = Duration::from_secs(240);

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn seeds_of(target: &str) -> Vec<(String, Vec<u8>)> {
    let directory = repository().join("fuzz/corpus").join(target);
    let mut seeds: Vec<_> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    seeds.sort();
    seeds
}

struct Mutator(u64);

impl Mutator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }

    fn mutate(&mut self, seed: &[u8]) -> Vec<u8> {
        let mut input = seed.to_vec();
        for _ in 0..=self.below(8) {
            let at = self.below(input.len());
            let byte = self.next() as u8;
            match self.below(6) {
                0 if !input.is_empty() => input[at] = byte,
                1 if !input.is_empty() => input[at] ^= 1 << (byte % 8),
                2 if !input.is_empty() => {
                    input[at] = [0, 1, 0x7f, 0x80, 0xff][usize::from(byte) % 5]
                }
                3 => input.insert(at, byte),
                4 if !input.is_empty() => {
                    input.remove(at);
                }
                5 => input.truncate(at),
                _ => {}
            }
        }
        input
    }
}

fn exercise(target: &'static Target) {
    let seeds = seeds_of(target.name);
    assert!(
        !seeds.is_empty(),
        "fuzz/corpus/{} has no seeds; chore test:oracle regenerates them with EXFAT_FUZZ_SEEDS_OUT",
        target.name
    );
    let (done, finished) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut mutator = Mutator(SEED);
        let mut cases = 0;
        let mut accepted = 0;
        let case = |input: &[u8], origin: &str, name: &str| {
            let run = std::panic::AssertUnwindSafe(|| (target.run)(input));
            match std::panic::catch_unwind(run) {
                Ok(result) => result,
                Err(_) => panic!("{}: seed {name}, {origin}, input {input:02x?}", target.name),
            }
        };
        for (name, seed) in &seeds {
            accepted += usize::from(case(seed, "as committed", name));
            cases += 1;
            for round in 0..target.rounds {
                case(&mutator.mutate(seed), &format!("round {round}"), name);
                cases += 1;
            }
        }
        done.send((cases, accepted, seeds.len())).unwrap();
    });
    match finished.recv_timeout(DEADLINE) {
        Ok((cases, accepted, count)) => {
            worker.join().unwrap();
            println!("{}: {cases} cases over {count} seeds", target.name);
            // A committed reproducer is usually refused, so one acceptance is the bar:
            // a corpus with none never gets past the decoder's first check.
            assert!(
                accepted > 0,
                "{}: none of its {count} seeds got past the decoder's first check",
                target.name
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => worker.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{} hung: still running after {DEADLINE:?}", target.name)
        }
    }
}

fn target(name: &str) -> &'static Target {
    TARGETS.iter().find(|target| target.name == name).unwrap()
}

macro_rules! replays {
    ($($name:ident),* $(,)?) => {
        $(#[test]
        fn $name() {
            exercise(target(stringify!($name)));
        })*
    };
}

replays!(
    boot_sector,
    fat_entry,
    allocation_bitmap,
    upcase_table,
    root_metadata,
    directory,
    timestamp,
    timestamps,
    image,
);

#[test]
fn the_image_target_starts_from_a_healthy_volume_with_every_kind_of_entry() {
    let report = shared::pristine_report();
    assert_eq!((report.files, report.directories), (5, 3));
    assert!(report.healthy(), "{report:?}");
    assert!(report.boot_differences.is_empty());
}

/// Each public `parse` or `decode` in a source file, named the way a caller writes it.
fn decoders_in(source: &str, module: &str) -> Vec<String> {
    let mut decoders = Vec::new();
    let mut owner: Option<String> = None;
    for line in source.lines() {
        if let Some(header) = line.strip_prefix("impl") {
            let mut depth = 0;
            let name: String = header
                .chars()
                .skip_while(|unit| {
                    match unit {
                        '<' => depth += 1,
                        '>' => depth -= 1,
                        _ => {}
                    }
                    depth > 0 || *unit == '>' || *unit == ' '
                })
                .take_while(|unit| unit.is_alphanumeric() || *unit == '_')
                .collect();
            owner = Some(name);
        } else if line.starts_with('}') {
            owner = None;
        }
        let trimmed = line.trim_start();
        for function in ["parse", "decode"] {
            if trimmed.starts_with(&format!("pub fn {function}(")) {
                let scope = owner.as_deref().unwrap_or(module);
                decoders.push(format!("{scope}::{function}"));
            }
        }
    }
    decoders
}

#[test]
fn the_scan_finds_associated_and_free_decoders() {
    let source = "impl<'a, T: Bound<'a>> Thing<'a, T> {\n    pub fn parse(data: &[u8]) {}\n    \
                  fn decode() {}\n}\n\npub fn decode(data: &[u8]) {}\npub fn parse_more() {}\n";
    assert_eq!(
        decoders_in(source, "module"),
        ["Thing::parse", "module::decode"]
    );
}

#[test]
fn every_decoder_has_a_target_and_every_target_has_a_binary_and_seeds() {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(repository().join("src")).unwrap() {
        let path = entry.unwrap().path();
        let module = path.file_stem().unwrap().to_string_lossy().into_owned();
        found.extend(decoders_in(&read(&path), &module));
    }
    found.sort();
    let mut covered: Vec<String> = TARGETS
        .iter()
        .flat_map(|target| target.decoders.iter().map(|name| name.to_string()))
        .collect();
    covered.sort();
    assert_eq!(found, covered, "a public decoder has no fuzz target");

    let shared_source = read(&repository().join("fuzz/shared/mod.rs"));
    let manifest = read(&repository().join("fuzz/Cargo.toml"));
    let binaries = manifest.matches("[[bin]]").count();
    assert_eq!(
        binaries,
        TARGETS.len(),
        "fuzz/Cargo.toml and TARGETS differ"
    );
    for target in TARGETS {
        for decoder in target.decoders {
            assert!(
                shared_source.contains(&format!("{decoder}(")),
                "{}: fuzz/shared/mod.rs never calls {decoder}",
                target.name
            );
        }
        assert!(
            manifest.contains(&format!("name = \"{}\"\n", target.name)),
            "{}: not declared in fuzz/Cargo.toml",
            target.name
        );
        let source = format!("fuzz/fuzz_targets/{}.rs", target.name);
        assert!(
            read(&repository().join(&source)).contains(&format!("shared::{}(data)", target.name)),
            "{source} does not run shared::{}",
            target.name
        );
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
