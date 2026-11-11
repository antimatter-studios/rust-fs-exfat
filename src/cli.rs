use crate::check;
use crate::device::AlignedDevice;
use crate::directory::DirectoryEntry;
use crate::file::File;
use crate::interface;
use crate::path::Node;
use crate::primitives::{DeviationReport, UninterpretedCounts};
use crate::salvage::Severity;
use crate::timestamp::Timestamp;
use crate::write;
use crate::Error;
use fs_core::cli::clap::{value_parser, Arg, ArgAction, ArgMatches, Command};
use fs_core::cli::{self, transfer, CliError, Family, Json, Outcome, Tool};
use fs_core::{FileDevice, OwnedSlice};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;

mod mkfs;

pub static FAMILY: Family = Family {
    repo: "rust-fs-exfat",
    crate_name: "rust-fs-exfat",
    version: env!("CARGO_PKG_VERSION"),
    about: "exFAT inspection, extraction, writing and formatting",
    install_hints: &["cargo install rust-fs-exfat --features cli"],
    tools: &[
        Tool {
            name: "fs.exfat",
            verb: "fs",
            section: 1,
            about: "List, inspect, extract and write files without mounting",
            usage_exit: 2,
            command: fs_command,
            run: fs_run,
        },
        Tool {
            name: "fsck.exfat",
            verb: "fsck",
            section: 8,
            about: "Check exFAT read-only; never repair",
            usage_exit: 16,
            command: fsck_command,
            run: fsck_run,
        },
        Tool {
            name: "mkfs.exfat",
            verb: "mkfs",
            section: 8,
            about: "Create an empty exFAT volume on a device or an image file",
            usage_exit: fs_core::cli::output::EXIT_USAGE,
            command: mkfs::command,
            run: mkfs::run,
        },
    ],
};

fn target() -> Arg {
    Arg::new("target")
        .value_name("IMAGE")
        .help("The image or device; opened read-only except by the writing verbs")
        .required(true)
        .value_parser(value_parser!(OsString))
}

fn offset() -> Arg {
    Arg::new("offset")
        .long("offset")
        .value_name("BYTES")
        .help("512-byte-aligned filesystem offset within IMAGE")
        .default_value("0")
        .global(true)
        .value_parser(value_parser!(u64))
}

fn path(required: bool) -> Arg {
    let argument = Arg::new("path")
        .value_name("PATH")
        .value_parser(value_parser!(OsString));
    if required {
        argument.required(true)
    } else {
        argument.default_value("/")
    }
}

fn fs_command() -> Command {
    Command::new("fs.exfat")
        .about("Read and write an exFAT image without mounting it")
        .arg(target())
        .arg(offset())
        .args(cli::format_args().map(|argument| argument.global(true)))
        .subcommand_required(true)
        .subcommand(Command::new("ls").about("List a directory").arg(path(false)))
        .subcommand(Command::new("stat").about("Inspect a path and its timestamps").arg(path(true)))
        .subcommand(Command::new("cat").about("Write raw file bytes to stdout").arg(path(true)))
        .subcommand(Command::new("export").about("Atomically extract a file; --verify checks the staged bytes before publication").args(transfer::export_args_with_verify()))
        .subcommand(
            Command::new("mkdir")
                .about("Create a directory (its parent must exist, or with -p every one on the way)")
                .arg(Arg::new("parents").short('p').long("parents").action(ArgAction::SetTrue).help("Make every missing directory on the way; one already there is success"))
                .arg(path(true)),
        )
        .subcommand(
            Command::new("write")
                .about("Create or replace a file with the bytes on stdin, or write them into one at a position")
                .arg(Arg::new("at").long("at").value_name("BYTES").value_parser(value_parser!(u64)).help("Write into the existing file starting this many bytes in, truncating nothing"))
                .arg(path(true)),
        )
        .subcommand(Command::new("truncate").about("Set a file's length; growing reads as zeros").arg(Arg::new("size").long("size").value_name("BYTES").required(true).value_parser(value_parser!(u64))).arg(path(true)))
        .subcommand(Command::new("rm").about("Remove a file").arg(path(true)))
        .subcommand(Command::new("rmdir").about("Remove an empty directory").arg(path(true)))
        .subcommand(
            Command::new("mv")
                .about("Rename or move a file or directory, replacing a file or an empty directory as rename(2) does")
                .arg(path(true))
                .arg(Arg::new("to").value_name("TO").required(true).value_parser(value_parser!(OsString))),
        )
        .subcommand(
            Command::new("touch")
                .about("Set times, creating an empty file if PATH is missing; with no time given, modified and accessed become now")
                .arg(time_arg("created"))
                .arg(time_arg("modified"))
                .arg(time_arg("accessed"))
                .arg(Arg::new("utc-offset").long("utc-offset").value_name("MINUTES").default_value("0").allow_hyphen_values(true).value_parser(value_parser!(i16)).help("The UTC offset recorded with the given times, a multiple of 15"))
                .arg(path(true)),
        )
        .subcommand(
            Command::new("attributes")
                .about("Set ReadOnly, Hidden, System and Archive; names not listed are cleared")
                .arg(Arg::new("set").long("set").value_name("LIST").required(true).help("Comma-separated: read-only, hidden, system, archive; empty for none"))
                .arg(path(true)),
        )
        .subcommand(
            Command::new("label")
                .about("Set the volume label (up to 11 characters; empty for none)")
                .arg(Arg::new("label").value_name("LABEL").required(true).allow_hyphen_values(true)),
        )
        .after_help("Examples:\n  fs.exfat image.dmg ls /\n  fs.exfat image.dmg stat /file.txt\n  fs.exfat image.dmg cat /file.txt\n  fs.exfat image.dmg export /file.txt output.txt --verify\n  fs.exfat image.dmg mkdir -p /backup/2026\n  fs.exfat image.dmg write /backup/2026/notes.txt < notes.txt\n  printf 'v2' | fs.exfat image.dmg write --at 4096 /data.bin\n  fs.exfat image.dmg truncate --size 0 /data.bin\n  fs.exfat image.dmg rm /data.bin\n  fs.exfat image.dmg rmdir /backup/2026\n  fs.exfat image.dmg mv /notes.txt /backup/notes.txt\n  fs.exfat image.dmg touch --modified 1700000000120 /backup/notes.txt\n  fs.exfat image.dmg attributes --set hidden,read-only /backup/notes.txt\n  fs.exfat image.dmg label BACKUP\n\nls, stat, cat and export open the image read-only. mkdir, write, truncate, rm, rmdir, mv, touch, attributes and label open it for writing: the volume is marked dirty while they run and clean when they finish, and a volume already marked dirty is refused. write without --at creates the file or replaces its contents; with --at the file must exist, and a gap past its end reads back as zeros. mkfs.exfat formats new volumes.")
}

fn fsck_command() -> Command {
    Command::new("fsck.exfat")
        .about("Check exFAT read-only; never repairs or writes to the source")
        .arg(target())
        .arg(offset())
        .arg(Arg::new("no-repair").short('n').long("no-repair").action(ArgAction::SetTrue).help("Check only (always enabled; repairs are impossible)"))
        .args(cli::format_args())
        .after_help("Checks both boot copies, system metadata, directory entry sets, allocation ownership and file chains.\nCounts vendor and unknown directory entries it does not interpret, without failing on them.\nLists deviations from the exFAT specification with byte offset, section and severity, without failing on them.\nAll exFAT cluster counts are supported: bitmap and ownership use 1/4 byte per cluster, with a 1,537 MiB audit payload ceiling.\nFAT and directories are streamed in 64 KiB chunks; directory workspace is budgeted, not entry-count limited.\nAn exhausted budget or incomplete audit fails explicitly; no partial clean result is emitted.\nExit codes: 0 clean, 4 uncorrected inconsistencies, 8 I/O or unsupported audit limits, 16 usage.\nThere is no repair mode.")
}

fn error(error: Error) -> CliError {
    match error {
        Error::NotFound => CliError::not_found(error.to_string()),
        _ => CliError::failed(error.to_string()),
    }
}

fn timestamp(value: Timestamp) -> Json {
    Json::object([
        ("year", Json::from(value.year)),
        ("month", Json::from(u64::from(value.month))),
        ("day", Json::from(u64::from(value.day))),
        ("hour", Json::from(u64::from(value.hour))),
        ("minute", Json::from(u64::from(value.minute))),
        ("second", Json::from(u64::from(value.second))),
        ("millisecond", Json::from(value.millisecond)),
        (
            "utc_offset_minutes",
            Json::from(value.utc_offset_minutes.map(i64::from)),
        ),
        ("unix_milliseconds", Json::from(value.unix_milliseconds())),
    ])
}

fn entry_json(entry: &DirectoryEntry) -> Json {
    Json::object([
        ("name", Json::from(entry.name())),
        (
            "type",
            Json::from(if entry.is_directory() {
                "directory"
            } else {
                "file"
            }),
        ),
        ("size", Json::from(entry.stream().data_length())),
        (
            "valid_data_length",
            Json::from(entry.stream().valid_data_length()),
        ),
        ("first_cluster", Json::from(entry.stream().first_cluster())),
        ("attributes", Json::from(entry.attributes())),
        ("no_fat_chain", Json::from(entry.stream().no_fat_chain())),
        ("created", timestamp(entry.timestamps().created)),
        ("modified", timestamp(entry.timestamps().modified)),
        ("accessed", timestamp(entry.timestamps().accessed)),
    ])
}

struct ExportSource<'volume> {
    file: File<'volume, OwnedSlice>,
    image: &'volume Path,
}

impl transfer::FileSource for ExportSource<'_> {
    fn image_path(&self) -> Option<&Path> {
        Some(self.image)
    }

    fn size(&self) -> u64 {
        self.file.len()
    }

    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, CliError> {
        self.file.read_at(offset, output).map_err(error)
    }
}

fn fs_run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let image = Path::new(matches.get_one::<OsString>("target").unwrap());
    let offset = *matches.get_one::<u64>("offset").unwrap();
    if !offset.is_multiple_of(512) {
        return Err(CliError::usage("--offset must be aligned to 512 bytes"));
    }
    let (verb, arguments) = matches
        .subcommand()
        .ok_or_else(|| CliError::usage("missing verb"))?;
    if verb == "label" {
        let label = arguments.get_one::<String>("label").unwrap();
        let mut volume = write::open_path(image, offset).map_err(error)?;
        volume.set_label(label).map_err(error)?;
        volume.unmount().map_err(error)?;
        return Ok(Outcome::report(Json::object([(
            "label",
            Json::from(label.as_str()),
        )])));
    }
    let requested = arguments.get_one::<OsString>("path").unwrap();
    let path = requested
        .to_str()
        .ok_or_else(|| CliError::usage("exFAT paths must be UTF-8"))?;
    if matches!(
        verb,
        "mkdir" | "write" | "truncate" | "rm" | "rmdir" | "mv" | "touch" | "attributes"
    ) {
        return write_run(image, offset, verb, path, arguments);
    }
    let export = if verb == "export" {
        Some((
            transfer::ExportRequest::from_matches(arguments)?,
            transfer::ExportOptions::from_matches(arguments)?,
        ))
    } else {
        None
    };
    let volume = interface::open_path(image, offset).map_err(error)?;
    match verb {
        "ls" => Ok(Outcome::report(Json::Arr(
            volume
                .read_directory_path(path)
                .map_err(error)?
                .iter()
                .map(entry_json)
                .collect(),
        ))),
        "stat" => {
            let node = volume.lookup(path).map_err(error)?;
            let report = match node {
                Node::Entry(entry) => entry_json(&entry),
                Node::Root => Json::object([
                    ("name", Json::from("/")),
                    ("type", Json::from("directory")),
                    ("first_cluster", Json::from(volume.boot().root_cluster)),
                    ("size", Json::from(0u64)),
                    ("created", Json::Null),
                    ("modified", Json::Null),
                    ("accessed", Json::Null),
                ]),
            };
            Ok(Outcome::report(report))
        }
        "cat" | "export" => {
            let entry = match volume.lookup(path).map_err(error)? {
                Node::Entry(entry) if !entry.is_directory() => entry,
                _ => return Err(CliError::failed("path is a directory")),
            };
            let file = volume.open_file(&entry).map_err(error)?;
            if let Some((request, options)) = export {
                return transfer::export_with_options(
                    &request,
                    &mut ExportSource { file, image },
                    options,
                );
            }
            let mut stdout = std::io::stdout().lock();
            let mut buffer = vec![0; transfer::CHUNK];
            let mut position = 0;
            while position < file.len() {
                let count = file.read_at(position, &mut buffer).map_err(error)?;
                if count == 0 {
                    return Err(CliError::failed("short file read"));
                }
                stdout
                    .write_all(&buffer[..count])
                    .map_err(|problem| CliError::failed(problem.to_string()))?;
                position += count as u64;
            }
            stdout
                .flush()
                .map_err(|problem| CliError::failed(problem.to_string()))?;
            Ok(Outcome::done())
        }
        _ => Err(CliError::usage("unknown verb")),
    }
}

fn write_run(
    image: &Path,
    offset: u64,
    verb: &str,
    path: &str,
    arguments: &ArgMatches,
) -> Result<Outcome, CliError> {
    let mut volume = write::open_path(image, offset).map_err(error)?;
    let report = match verb {
        "mkdir" if arguments.get_flag("parents") => {
            let mut made = Vec::new();
            let mut current = String::new();
            for part in path.split('/').filter(|part| !part.is_empty()) {
                current.push('/');
                current.push_str(part);
                match volume.reader().lookup(&current) {
                    Ok(node) if node.is_directory() => continue,
                    Ok(_) => return Err(error(Error::NotDirectory)),
                    Err(Error::NotFound) => {}
                    Err(problem) => return Err(error(problem)),
                }
                volume.mkdir(&current).map_err(error)?;
                made.push(Json::from(current.as_str()));
            }
            Json::object([("created", Json::Arr(made))])
        }
        "mkdir" => {
            volume.mkdir(path).map_err(error)?;
            Json::object([("created", Json::Arr(vec![Json::from(path)]))])
        }
        "truncate" => {
            let size = *arguments.get_one::<u64>("size").unwrap();
            volume.truncate(path, size).map_err(error)?;
            Json::object([("path", Json::from(path)), ("size", Json::from(size))])
        }
        "rm" => {
            volume.unlink(path).map_err(error)?;
            Json::object([("removed", Json::from(path))])
        }
        "rmdir" => {
            volume.rmdir(path).map_err(error)?;
            Json::object([("removed", Json::from(path))])
        }
        "mv" => {
            let to = arguments
                .get_one::<OsString>("to")
                .unwrap()
                .to_str()
                .ok_or_else(|| CliError::usage("exFAT paths must be UTF-8"))?;
            volume.rename(path, to).map_err(error)?;
            Json::object([("from", Json::from(path)), ("to", Json::from(to))])
        }
        "touch" => {
            let offset_minutes = *arguments.get_one::<i16>("utc-offset").unwrap();
            let given = |name: &str| -> Result<Option<Timestamp>, CliError> {
                arguments
                    .get_one::<i64>(name)
                    .map(|milliseconds| {
                        Timestamp::from_unix_milliseconds(*milliseconds, offset_minutes)
                            .map_err(error)
                    })
                    .transpose()
            };
            let mut times = write::SetTimes {
                created: given("created")?,
                modified: given("modified")?,
                accessed: given("accessed")?,
            };
            if times == write::SetTimes::default() {
                let now = Timestamp::now().map_err(error)?;
                times.modified = Some(now);
                times.accessed = Some(now);
            }
            match volume.reader().lookup(path) {
                Err(Error::NotFound) => volume.create(path).map_err(error)?,
                Err(problem) => return Err(error(problem)),
                Ok(_) => {}
            }
            volume.set_times(path, times).map_err(error)?;
            Json::object([("path", Json::from(path))])
        }
        "attributes" => {
            let mut value = 0u16;
            let list = arguments.get_one::<String>("set").unwrap();
            for name in list.split(',').filter(|name| !name.is_empty()) {
                value |= match name {
                    "read-only" => 0x01,
                    "hidden" => 0x02,
                    "system" => 0x04,
                    "archive" => 0x20,
                    _ => {
                        return Err(CliError::usage(
                            "attributes are read-only, hidden, system and archive",
                        ))
                    }
                };
            }
            volume.set_attributes(path, value).map_err(error)?;
            Json::object([
                ("path", Json::from(path)),
                ("attributes", Json::from(value)),
            ])
        }
        _ => {
            let start = arguments.get_one::<u64>("at").copied();
            if start.is_none() {
                volume.write_file(path, &[]).map_err(error)?;
            }
            let mut position = start.unwrap_or(0);
            let mut buffer = vec![0; transfer::CHUNK];
            let mut input = std::io::stdin().lock();
            let mut total = 0u64;
            loop {
                let count = input
                    .read(&mut buffer)
                    .map_err(|problem| CliError::failed(problem.to_string()))?;
                if count == 0 {
                    break;
                }
                volume
                    .pwrite(path, position, &buffer[..count])
                    .map_err(error)?;
                position += count as u64;
                total += count as u64;
            }
            if start.is_some() && total == 0 {
                // An empty write still names a file that must exist.
                volume.pwrite(path, position, &[]).map_err(error)?;
            }
            Json::object([("path", Json::from(path)), ("written", Json::from(total))])
        }
    };
    volume.unmount().map_err(error)?;
    Ok(Outcome::report(report))
}

fn time_arg(name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name("UNIX_MS")
        .allow_hyphen_values(true)
        .value_parser(value_parser!(i64))
        .help("Milliseconds since the Unix epoch; exFAT keeps hundredths")
}

fn fsck_error(problem: Error) -> CliError {
    let code = match problem {
        Error::Device(_) | Error::Unsupported(_) => 8,
        _ => 4,
    };
    CliError::failed(problem.to_string()).with_code(code)
}

/// Counts of entries the check does not interpret, which never fail it.
fn uninterpreted_json(counts: UninterpretedCounts) -> Json {
    Json::object([
        ("vendor_extension", Json::from(counts.vendor_extension)),
        ("vendor_allocation", Json::from(counts.vendor_allocation)),
        (
            "unknown_benign_primary",
            Json::from(counts.unknown_benign_primary),
        ),
        (
            "unknown_critical_primary",
            Json::from(counts.unknown_critical_primary),
        ),
        (
            "unknown_critical_secondary",
            Json::from(counts.unknown_critical_secondary),
        ),
        (
            "unknown_benign_secondary",
            Json::from(counts.unknown_benign_secondary),
        ),
    ])
}

/// Spec deviations, apart from the verdict: each with its volume byte offset,
/// section and severity (`docs/spec-rules.md`).
fn deviations_json(report: &DeviationReport) -> Json {
    let found = report
        .found
        .iter()
        .map(|deviation| {
            Json::object([
                ("rule", Json::from(deviation.rule.id())),
                ("section", Json::from(deviation.section)),
                (
                    "severity",
                    Json::from(match deviation.severity {
                        Severity::Notice => "notice",
                        Severity::Warning => "warning",
                        Severity::Error => "error",
                    }),
                ),
                ("offset", Json::from(deviation.offset)),
                ("length", Json::from(deviation.length)),
                ("detail", Json::from(deviation.detail.as_str())),
            ])
        })
        .collect();
    Json::object([
        ("found", Json::Arr(found)),
        (
            "omitted",
            Json::object(
                report
                    .omitted
                    .iter()
                    .map(|(rule, count)| (rule.id(), Json::from(*count))),
            ),
        ),
        ("complete", Json::from(report.complete)),
        (
            "skipped",
            Json::Arr(
                report
                    .skipped
                    .iter()
                    .map(|part| Json::from(part.as_str()))
                    .collect(),
            ),
        ),
        ("skipped_omitted", Json::from(report.skipped_omitted)),
    ])
}

fn fsck_run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let image = Path::new(matches.get_one::<OsString>("target").unwrap());
    let offset = *matches.get_one::<u64>("offset").unwrap();
    if !offset.is_multiple_of(512) {
        return Err(CliError::usage("--offset must be aligned to 512 bytes").with_code(16));
    }
    let device = FileDevice::open(image).map_err(|problem| fsck_error(problem.into()))?;
    let length = fs_core::BlockRead::size_bytes(&device);
    if offset >= length {
        return Err(fsck_error(Error::Invalid(
            "volume offset is outside the device",
        )));
    }
    let device = std::sync::Arc::new(AlignedDevice::new(device));
    let report = check::check_device(OwnedSlice::new(device, offset, length - offset))
        .map_err(fsck_error)?;
    let healthy = report.healthy();
    let differences = report
        .boot_differences
        .iter()
        .map(|difference| {
            Json::object([
                ("offset", Json::from(difference.offset as u64)),
                ("main", Json::from(u64::from(difference.main))),
                ("backup", Json::from(u64::from(difference.backup))),
            ])
        })
        .collect();
    Ok(Outcome::report(Json::object([
        ("check_only", Json::from(true)),
        ("healthy", Json::from(healthy)),
        ("files", Json::from(report.files)),
        ("directories", Json::from(report.directories)),
        ("allocated_clusters", Json::from(report.allocated_clusters)),
        (
            "referenced_clusters",
            Json::from(report.referenced_clusters),
        ),
        ("bad_clusters", Json::from(report.bad_clusters)),
        (
            "unreferenced_clusters",
            Json::from(report.unreferenced_clusters),
        ),
        ("dirty", Json::from(report.dirty)),
        ("media_failure", Json::from(report.media_failure)),
        ("boot_differences", Json::Arr(differences)),
        (
            "uninterpreted_entries",
            uninterpreted_json(report.uninterpreted),
        ),
        ("deviations", deviations_json(&report.deviations)),
    ]))
    .with_code(if healthy { 0 } else { 4 }))
}
