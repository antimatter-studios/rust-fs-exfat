//! `mkfs.exfat`: create an empty exFAT volume on an image file or device.
//!
//! It writes only to its target. The JSON report is read back
//! from the new volume by the strict reader, so a report is also proof the
//! reader accepts what was written; `--dry-run` reports the plan and writes
//! nothing.

use crate::interface;
use crate::mkfs::{self, FormatError, FormatOptions, Layout, MAX_LABEL_UNITS};
use crate::Error;
use fs_core::cli::clap::{value_parser, Arg, ArgAction, ArgMatches, Command};
use fs_core::cli::{self, CliError, Json, Outcome};
use fs_core::{BlockRead, FileDevice};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::path::Path;

pub(super) fn command() -> Command {
    Command::new("mkfs.exfat")
        .about("Create an empty exFAT volume on a device or an image file")
        .long_about(
            "Create an empty exFAT volume on a device or a pre-sized image file.\n\n\
             The target must already exist at the volume's size (`truncate -s 64M out.img`), \
             unless --size is given for an image file that does not exist yet.\n\n\
             A JSON report of what was written, read back from the new volume, goes to stdout.",
        )
        .arg(
            Arg::new("target")
                .value_name("TARGET")
                .help("Image file or device to format")
                .required(true)
                .value_parser(value_parser!(OsString)),
        )
        .arg(
            Arg::new("label")
                .short('L')
                .long("label")
                .value_name("LABEL")
                .help(format!(
                    "Volume label: at most {MAX_LABEL_UNITS} UTF-16 code units, without control \
                     codes or \" * / : < > ? \\ |"
                )),
        )
        .arg(
            Arg::new("sector-size")
                .short('S')
                .long("sector-size")
                .value_name("BYTES")
                .help("Bytes per sector: 512, 1024, 2048 or 4096. Default: 512.")
                .value_parser(value_parser!(u32)),
        )
        .arg(
            Arg::new("cluster-size")
                .short('c')
                .long("cluster-size")
                .value_name("BYTES")
                .help(
                    "Bytes per cluster, a power of two up to 32M (K/M suffixes). Default: by \
                     volume size, 4K below 256M, 32K below 32G, then 128K or more.",
                )
                .value_parser(parse_size),
        )
        .arg(
            Arg::new("alignment")
                .long("alignment")
                .value_name("BYTES")
                .help(
                    "Start the FAT and the cluster heap on multiples of BYTES, a multiple of the \
                     sector size (K/M suffixes). Default: the cluster size.",
                )
                .value_parser(parse_size),
        )
        .arg(
            Arg::new("fats")
                .long("fats")
                .value_name("COUNT")
                .help("1, or 2 for a TexFAT volume with a second FAT and Allocation Bitmap")
                .default_value("1")
                .value_parser(value_parser!(u8).range(1..=2)),
        )
        .arg(
            Arg::new("serial")
                .long("serial")
                .value_name("HEX")
                .help("Volume serial number, 8 hex digits. Default: from the date and time.")
                .value_parser(parse_serial),
        )
        .arg(
            Arg::new("guid")
                .long("guid")
                .value_name("GUID")
                .help("Write a Volume GUID entry: 32 hex digits, dashes optional")
                .value_parser(parse_guid),
        )
        .arg(
            Arg::new("partition-offset")
                .long("partition-offset")
                .value_name("SECTORS")
                .help("PartitionOffset to record, in sectors. Default: 0, which readers ignore.")
                .default_value("0")
                .value_parser(value_parser!(u64)),
        )
        .arg(
            Arg::new("size")
                .long("size")
                .value_name("SIZE")
                .help(
                    "Format SIZE bytes (K/M/G/T suffixes, 1024-based). A TARGET that does not \
                     exist is created as an image file of SIZE bytes first.",
                )
                .value_parser(parse_size),
        )
        .arg(
            Arg::new("dry-run")
                .short('n')
                .long("dry-run")
                .help("Report the layout and write nothing")
                .action(ArgAction::SetTrue),
        )
        .args(cli::format_args())
        .after_help(
            "Examples:\n  \
             mkfs.exfat --size 64M --label BACKUP disk.img\n  \
             truncate -s 8G disk.img && mkfs.exfat -S 4096 disk.img\n  \
             mkfs.exfat -n disk.img                 report the layout, write nothing\n\n\
             It writes only to TARGET, and replaces any volume there. OEM parameters \
             of an exFAT volume already on TARGET are kept, as the specification \
             requires of a later format.",
        )
}

/// A byte count with an optional K, M, G or T suffix, 1024-based.
fn parse_size(value: &str) -> Result<u64, String> {
    let (digits, shift) = match value.char_indices().last() {
        Some((index, suffix)) if suffix.is_ascii_alphabetic() => {
            let shift = match suffix.to_ascii_uppercase() {
                'K' => 10,
                'M' => 20,
                'G' => 30,
                'T' => 40,
                _ => return Err(format!("unknown size suffix {suffix:?}")),
            };
            (&value[..index], shift)
        }
        _ => (value, 0),
    };
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("{value:?} is not a byte count"))?;
    number
        .checked_mul(1 << shift)
        .ok_or_else(|| format!("{value:?} is too large"))
}

fn parse_serial(value: &str) -> Result<u32, String> {
    let digits = value.strip_prefix("0x").unwrap_or(value);
    u32::from_str_radix(digits, 16).map_err(|_| format!("{value:?} is not 8 hex digits"))
}

fn parse_guid(value: &str) -> Result<[u8; 16], String> {
    let digits: String = value
        .chars()
        .filter(|&character| character != '-')
        .collect();
    if digits.len() != 32
        || !digits
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!("{value:?} is not 32 hex digits"));
    }
    let mut guid = [0; 16];
    for (index, byte) in guid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).unwrap();
    }
    Ok(guid)
}

fn refused(error: FormatError) -> CliError {
    match error {
        FormatError::Refused(_) => CliError::usage(error.to_string()),
        _ => CliError::failed(error.to_string()),
    }
}

fn failed(error: Error) -> CliError {
    CliError::failed(error.to_string())
}

fn io_failed(error: std::io::Error, target: &Path) -> CliError {
    CliError::failed(format!("{}: {error}", target.display()))
}

fn options(matches: &ArgMatches) -> Result<FormatOptions, CliError> {
    let narrow = |name: &str| -> Result<Option<u32>, CliError> {
        matches
            .get_one::<u64>(name)
            .map(|&value| {
                u32::try_from(value)
                    .map_err(|_| CliError::usage(format!("--{name} {value} is too large")))
            })
            .transpose()
    };
    Ok(FormatOptions {
        size_bytes: matches.get_one::<u64>("size").copied(),
        bytes_per_sector: matches.get_one::<u32>("sector-size").copied(),
        cluster_size: narrow("cluster-size")?,
        alignment: matches.get_one::<u64>("alignment").copied(),
        number_of_fats: *matches.get_one::<u8>("fats").unwrap(),
        label: matches.get_one::<String>("label").cloned(),
        volume_serial: matches.get_one::<u32>("serial").copied(),
        guid: matches.get_one::<[u8; 16]>("guid").copied(),
        partition_offset: *matches.get_one::<u64>("partition-offset").unwrap(),
    })
}

fn guid_text(guid: Option<[u8; 16]>) -> Json {
    match guid {
        Some(guid) => Json::from(
            guid.iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        None => Json::Null,
    }
}

fn report(layout: &Layout, label: Option<String>, guid: Option<[u8; 16]>, dry_run: bool) -> Json {
    Json::object([
        ("dry_run", Json::from(dry_run)),
        ("bytes_per_sector", Json::from(layout.bytes_per_sector())),
        ("cluster_size", Json::from(layout.cluster_size())),
        ("volume_length", Json::from(layout.volume_length)),
        ("partition_offset", Json::from(layout.partition_offset)),
        ("fat_offset", Json::from(layout.fat_offset)),
        ("fat_length", Json::from(layout.fat_length)),
        (
            "number_of_fats",
            Json::from(u64::from(layout.number_of_fats)),
        ),
        (
            "cluster_heap_offset",
            Json::from(layout.cluster_heap_offset),
        ),
        ("cluster_count", Json::from(layout.cluster_count)),
        ("root_cluster", Json::from(layout.root.first_cluster)),
        (
            "volume_serial",
            Json::from(format!("{:08x}", layout.volume_serial)),
        ),
        (
            "percent_in_use",
            Json::from(u64::from(layout.percent_in_use)),
        ),
        ("label", label.map_or(Json::Null, Json::from)),
        ("guid", guid_text(guid)),
        (
            "oem_parameters_preserved",
            Json::from(layout.oem_parameters_preserved),
        ),
    ])
}

pub(super) fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let target = Path::new(matches.get_one::<OsString>("target").unwrap());
    let options = options(matches)?;
    let dry_run = matches.get_flag("dry-run");
    let exists = target.exists();
    if !exists && options.size_bytes.is_none() {
        return Err(CliError::usage(format!(
            "{} does not exist; give --size to create it",
            target.display()
        )));
    }
    if dry_run {
        let size = match options.size_bytes {
            Some(size) => size,
            None => FileDevice::open(target)
                .map_err(|error| failed(error.into()))?
                .size_bytes(),
        };
        let layout = mkfs::plan(size, &options).map_err(refused)?;
        let label = layout
            .label
            .as_ref()
            .map(|units| String::from_utf16_lossy(units));
        return Ok(Outcome::report(report(&layout, label, layout.guid, true)));
    }
    if !exists {
        // Checked before anything exists, so a refused layout leaves no file.
        mkfs::plan(options.size_bytes.unwrap(), &options).map_err(refused)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(|error| io_failed(error, target))?;
        file.set_len(options.size_bytes.unwrap())
            .map_err(|error| io_failed(error, target))?;
    }
    let device = FileDevice::open_rw(target).map_err(|error| failed(error.into()))?;
    let layout = mkfs::format(&device, &options).map_err(refused)?;
    drop(device);
    // The report comes from the volume as the strict reader sees it.
    let volume = interface::open_path(target, 0).map_err(failed)?;
    let boot = volume.boot();
    let metadata = volume.metadata();
    if boot.cluster_count != layout.cluster_count || boot.volume_serial != layout.volume_serial {
        return Err(CliError::failed(
            "the volume read back differs from the one written",
        ));
    }
    Ok(Outcome::report(report(
        &layout,
        metadata.label.clone(),
        metadata.guid,
        false,
    )))
}
