mod interface_support;

use serde_json::Value;
use std::process::{Command, Output};

fn run(image: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .arg("fs")
        .arg(image)
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn listing_stat_and_cat_preserve_metadata_and_zero_filling() {
    let fixture = interface_support::Temp::new();
    let listing = run(&fixture.image, &["ls", "/"]);
    assert!(
        listing.status.success(),
        "{}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let entries: Value = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 3);
    let metadata = run(&fixture.image, &["stat", "/eXAMPLE.TXT"]);
    let metadata: Value = serde_json::from_slice(&metadata.stdout).unwrap();
    assert_eq!(metadata["size"], 1030);
    assert_eq!(metadata["valid_data_length"], 513);
    assert_eq!(metadata["created"]["year"], 2026);
    let contents = run(&fixture.image, &["cat", "Example.txt"]);
    assert!(contents.status.success());
    assert_eq!(contents.stdout, interface_support::expected());
    assert!(run(&fixture.image, &["cat", "Empty.bin"]).stdout.is_empty());
    assert_eq!(
        run(&fixture.image, &["cat", "Sub/Child.bin"]).stdout,
        b"nested\n"
    );
}

#[test]
fn verified_export_reports_digest_and_never_replaces_the_source() {
    let fixture = interface_support::Temp::new();
    let original = std::fs::read(&fixture.image).unwrap();
    let destination = fixture.directory.join("result.bin");
    let output = run(
        &fixture.image,
        &[
            "export",
            "Example.txt",
            destination.to_str().unwrap(),
            "--verify",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["verified"], true);
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        interface_support::expected()
    );
    assert!(!run(
        &fixture.image,
        &[
            "export",
            "Example.txt",
            destination.to_str().unwrap(),
            "--verify"
        ]
    )
    .status
    .success());
    assert!(run(
        &fixture.image,
        &[
            "export",
            "Example.txt",
            destination.to_str().unwrap(),
            "--verify",
            "--replace"
        ]
    )
    .status
    .success());
    assert!(!run(
        &fixture.image,
        &[
            "export",
            "Example.txt",
            fixture.image.to_str().unwrap(),
            "--replace",
            "--verify"
        ]
    )
    .status
    .success());
    assert_eq!(
        run(&fixture.image, &["export", "Example.txt", "-", "--verify"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(std::fs::read(&fixture.image).unwrap(), original);
}

#[test]
fn errors_are_structured_and_read_only_modes_are_explicit() {
    let fixture = interface_support::Temp::new();
    let missing = run(&fixture.image, &["stat", "absent"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["kind"], "not_found");
    assert!(!run(&fixture.image, &["cat", "/"]).status.success());
    assert!(!run(&fixture.image, &["ls", "Example.txt"]).status.success());
    assert_eq!(
        run(&fixture.image, &["--offset", "1", "ls"]).status.code(),
        Some(2)
    );
    for name in ["fs.exfat", "fsck.exfat"] {
        let output = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
            .args([name, "--version"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains(name));
    }
    let help = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .args(["fsck", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("never repairs"));
}

#[test]
fn check_only_checker_reports_clean_and_dirty_without_changing_bytes() {
    let fixture = interface_support::Temp::new();
    let mut bytes = std::fs::read(&fixture.image).unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
            .arg("fsck")
            .arg(&fixture.image)
            .arg("-n")
            .output()
            .unwrap()
    };
    let output = check();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["check_only"], true);
    assert_eq!(report["files"], 3);
    bytes[106] |= 2;
    std::fs::write(&fixture.image, &bytes).unwrap();
    let dirty = check();
    assert_eq!(dirty.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&dirty.stdout).unwrap()["dirty"],
        true
    );
    let repair = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .arg("fsck")
        .arg(&fixture.image)
        .arg("--repair")
        .output()
        .unwrap();
    assert_eq!(repair.status.code(), Some(16));
    assert_eq!(std::fs::read(&fixture.image).unwrap(), bytes);
}

#[test]
fn fsck_counts_uninterpreted_entries_apart_from_its_verdict() {
    let fixture = interface_support::Temp::new();
    let mut bytes = std::fs::read(&fixture.image).unwrap();
    // A Vendor Extension entry (specification section 7.8) after the name
    // entry of Sub/Child.bin, which a reader may ignore.
    let mut child = interface_support::base::file_set("Child.bin", 0x20, 9, 7, true);
    child[1] += 1;
    let mut vendor = vec![0u8; 32];
    vendor[0] = 0xE0;
    vendor[2..18].fill(0x5A);
    child.extend_from_slice(&vendor);
    interface_support::base::seal_set(&mut child);
    let sub = 38 * 512;
    bytes[sub..sub + child.len()].copy_from_slice(&child);
    std::fs::write(&fixture.image, &bytes).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .arg("fsck")
        .arg(&fixture.image)
        .arg("-n")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["healthy"], true);
    let counts = &report["uninterpreted_entries"];
    assert_eq!(counts["vendor_extension"], 1);
    for kind in [
        "vendor_allocation",
        "unknown_benign_primary",
        "unknown_critical_primary",
        "unknown_critical_secondary",
        "unknown_benign_secondary",
    ] {
        assert_eq!(counts[kind], 0, "{kind}");
    }
}

#[test]
fn fsck_lists_spec_deviations_apart_from_its_verdict() {
    // The fixture's ClusterCount (100) is less than the 4064 clusters its
    // VolumeLength holds (specification section 3.1.9), in both boot sectors,
    // and its PercentInUse is 0 with 8 of 100 clusters allocated (3.1.18).
    let fixture = interface_support::Temp::new();
    let output = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .arg("fsck")
        .arg(&fixture.image)
        .arg("-n")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["healthy"], true);
    let deviations = &report["deviations"];
    let found: Vec<_> = deviations["found"]
        .as_array()
        .unwrap()
        .iter()
        .map(|deviation| {
            (
                deviation["rule"].as_str().unwrap(),
                deviation["section"].as_str().unwrap(),
                deviation["severity"].as_str().unwrap(),
                deviation["offset"].as_u64().unwrap(),
                deviation["length"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("cluster-count", "3.1.9", "warning", 92, 4),
            ("cluster-count", "3.1.9", "warning", 6236, 4),
            ("percent-in-use", "3.1.18", "warning", 112, 1),
        ]
    );
    assert!(deviations["found"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("4064"));
    assert_eq!(deviations["omitted"], serde_json::json!({}));
    assert_eq!(deviations["complete"], true);
    assert_eq!(deviations["skipped"], serde_json::json!([]));
    assert_eq!(deviations["skipped_omitted"], 0);
}
