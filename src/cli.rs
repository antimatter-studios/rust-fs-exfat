use crate::check;
use crate::device::AlignedDevice;
use crate::directory::DirectoryEntry;
use crate::file::File;
use crate::interface;
use crate::path::Node;
use crate::primitives::UninterpretedCounts;
use crate::timestamp::Timestamp;
use crate::Error;
use fs_core::cli::clap::{value_parser, Arg, ArgAction, ArgMatches, Command};
use fs_core::cli::{self, transfer, CliError, Family, Json, Outcome, Tool};
use fs_core::{FileDevice, OwnedSlice};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

pub static FAMILY: Family = Family {
    repo: "rust-fs-exfat",
    crate_name: "rust-fs-exfat",
    version: env!("CARGO_PKG_VERSION"),
    about: "Strict read-only exFAT inspection and extraction",
    install_hints: &["cargo install rust-fs-exfat --features cli"],
    tools: &[
        Tool {
            name: "fs.exfat",
            verb: "fs",
            section: 1,
            about: "List, inspect and extract files without mounting or writing to the source",
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
    ],
};

fn target() -> Arg {
    Arg::new("target")
        .value_name("IMAGE")
        .help("The source image or device, opened read-only")
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
        .about("Read an exFAT image without mounting or writing to it")
        .arg(target())
        .arg(offset())
        .args(cli::format_args().map(|argument| argument.global(true)))
        .subcommand_required(true)
        .subcommand(Command::new("ls").about("List a directory").arg(path(false)))
        .subcommand(Command::new("stat").about("Inspect a path and its timestamps").arg(path(true)))
        .subcommand(Command::new("cat").about("Write raw file bytes to stdout").arg(path(true)))
        .subcommand(Command::new("export").about("Atomically extract a file; --verify checks the staged bytes before publication").args(transfer::export_args_with_verify()))
        .after_help("Examples:\n  fs.exfat image.dmg ls /\n  fs.exfat image.dmg stat /file.txt\n  fs.exfat image.dmg cat /file.txt\n  fs.exfat image.dmg export /file.txt output.txt --verify\n\nThe source is always read-only. No formatter or writable mode exists.")
}

fn fsck_command() -> Command {
    Command::new("fsck.exfat")
        .about("Check exFAT read-only; never repairs or writes to the source")
        .arg(target())
        .arg(offset())
        .arg(Arg::new("no-repair").short('n').long("no-repair").action(ArgAction::SetTrue).help("Check only (always enabled; repairs are impossible)"))
        .args(cli::format_args())
        .after_help("Checks both boot copies, system metadata, directory entry sets, allocation ownership and file chains.\nCounts vendor and unknown directory entries it does not interpret, without failing on them.\nAll exFAT cluster counts are supported: bitmap and ownership use 1/4 byte per cluster, with a 1,537 MiB audit payload ceiling.\nFAT and directories are streamed in 64 KiB chunks; directory workspace is budgeted, not entry-count limited.\nAn exhausted budget or incomplete audit fails explicitly; no partial clean result is emitted.\nExit codes: 0 clean, 4 uncorrected inconsistencies, 8 I/O or unsupported audit limits, 16 usage.\nThere is no repair mode.")
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
    let requested = arguments.get_one::<OsString>("path").unwrap();
    let path = requested
        .to_str()
        .ok_or_else(|| CliError::usage("exFAT paths must be UTF-8"))?;
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
    ]))
    .with_code(if healthy { 0 } else { 4 }))
}
