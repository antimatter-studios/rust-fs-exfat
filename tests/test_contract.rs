//! The test contract, checked: THE LINUX ORACLES RUN IN THE HARNESS VM AND
//! NOWHERE ELSE, and no test announces a skip.
//!
//! Why the host is forbidden rather than second choice: exfatprogs on a
//! workstation is whatever that machine has, a different version per
//! developer and none on a Mac, and a loop mount needs root and the kernel's
//! exfat driver. One version, in one guest, answers the same way for
//! everyone. So a test that spawns a Linux exFAT tool itself, or drives the
//! harness's vm.sh around `tests/guest_support`, is refused here even when
//! it would work on the machine that wrote it. macOS's own tools
//! (newfs_exfat, fsck_exfat, hdiutil) are a different oracle, run on a Mac
//! host by `chore test:oracle-macos`, and are not covered by this rule.
//!
//! This file names the patterns it looks for without spelling them out, so
//! that it does not trip its own scan.

use std::path::{Path, PathBuf};

/// The Linux programs that belong to the guest: exfatprogs, the loop and
/// mount tools, and the third implementation's FUSE driver and checker.
fn linux_tools() -> Vec<String> {
    let mut tools: Vec<String> = ["mkfs", "fsck", "dump", "tune"]
        .iter()
        .map(|tool| format!("{tool}.{}", "exfat"))
        .collect();
    tools.extend(
        [
            ["exfat", "label"],
            ["los", "etup"],
            ["um", "ount"],
            ["exfat", "-fuse"],
            ["exfat", "fsck"],
            ["mount", ".exfat-fuse"],
        ]
        .iter()
        .map(|parts| parts.concat()),
    );
    tools.push(["mo", "unt"].concat());
    tools
}

fn sources(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Every test source but this file and the one module allowed to reach the
/// guest.
fn test_sources() -> Vec<(String, String)> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut paths = Vec::new();
    sources(&tests, &mut paths);
    let mut out: Vec<(String, String)> = paths
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&tests)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .filter(|(name, _)| name != "test_contract.rs" && !name.starts_with("guest_support/"))
        .collect();
    out.sort();
    assert!(
        out.len() > 20,
        "the scan found too few test sources: {}",
        out.len()
    );
    out
}

/// The string literal opening at `text[at..]`, if one does.
fn literal_at(text: &str, at: usize) -> Option<&str> {
    let rest = text[at..].trim_start();
    let body = rest
        .strip_prefix("r\"")
        .or_else(|| rest.strip_prefix('"'))?;
    body.split('"').next()
}

/// `Command::new(<literal>)` calls whose program is a Linux tool, by path or
/// by name.
fn tool_spawns(text: &str) -> Vec<String> {
    let spawn = ["Command", "::new("].concat();
    let tools = linux_tools();
    let mut found = Vec::new();
    for (at, _) in text.match_indices(&spawn) {
        if let Some(program) = literal_at(text, at + spawn.len()) {
            let name = program.rsplit('/').next().unwrap_or(program);
            if tools.iter().any(|tool| tool == name) {
                found.push(program.to_owned());
            }
        }
    }
    found
}

/// A host shell handed a script that runs a Linux tool: `bash -c "...fsck..."`
/// or the same through `sh`.
fn shell_spawns(text: &str) -> Vec<String> {
    let spawn = ["Command", "::new("].concat();
    let tools = linux_tools();
    let mut found = Vec::new();
    for (at, _) in text.match_indices(&spawn) {
        let Some(program) = literal_at(text, at + spawn.len()) else {
            continue;
        };
        if !matches!(
            program.rsplit('/').next(),
            Some("bash" | "sh" | "env" | "sudo")
        ) {
            continue;
        }
        let call = &text[at..text[at..].find(';').map_or(text.len(), |end| at + end)];
        for tool in &tools {
            if call
                .split(|c: char| !(c.is_alphanumeric() || "._-".contains(c)))
                .any(|word| word == tool)
            {
                found.push(format!("{program} running {tool}"));
            }
        }
    }
    found
}

/// The harness driven directly rather than through tests/guest_support.
fn harness_spawns(text: &str) -> Vec<String> {
    let script = ["vm", ".sh"].concat();
    let harness = ["fs-linux", "-test-harness"].concat();
    text.lines()
        .filter(|line| line.contains(&script) || line.contains(&harness))
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// A test that prints that it skipped, which reads like a pass.
fn announced_skips(text: &str) -> Vec<String> {
    let words = [["SKIP"].concat(), ["skip", "ping"].concat()];
    text.lines()
        .filter(|line| line.contains("print"))
        .filter(|line| words.iter().any(|word| line.contains(word.as_str())))
        .map(|line| line.trim().to_owned())
        .collect()
}

fn refuse(what: &str, scan: fn(&str) -> Vec<String>) {
    let mut problems = Vec::new();
    for (name, text) in test_sources() {
        for found in scan(&text) {
            problems.push(format!("tests/{name}: {found}"));
        }
    }
    assert!(
        problems.is_empty(),
        "{what}. Reach the guest through tests/guest_support (`guest`), which runs \
         the tool in the harness VM and fails, never skips, without it:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn no_test_runs_a_linux_exfat_tool_on_the_host() {
    refuse("a test spawns a Linux tool on the host", tool_spawns);
}

#[test]
fn no_test_hands_a_linux_tool_to_a_host_shell() {
    refuse(
        "a test runs a Linux tool through a host shell",
        shell_spawns,
    );
}

#[test]
fn no_test_drives_the_harness_itself() {
    refuse(
        "a test drives the harness VM around tests/guest_support",
        harness_spawns,
    );
}

#[test]
fn no_test_announces_a_skip() {
    refuse("a test announces a skip", announced_skips);
}

#[test]
fn every_linux_oracle_test_reaches_the_guest_through_guest_support() {
    let call = ["guest", "("].concat();
    let module = ["mod guest", "_support"].concat();
    for (name, text) in test_sources() {
        if text.contains(&call) && !name.contains('/') {
            assert!(
                text.contains(&module),
                "tests/{name} calls a guest helper without tests/guest_support"
            );
        }
    }
}

#[test]
fn the_scans_recognise_the_shapes_they_refuse() {
    let spawn = ["Command", "::new"].concat();
    let fsck = ["fsck.", "exfat"].concat();
    let direct = format!("let out = {spawn}(\"{fsck}\").arg(\"-n\");");
    assert_eq!(tool_spawns(&direct), std::slice::from_ref(&fsck));
    let by_path = format!("{spawn}(\"/usr/sbin/{fsck}\")");
    assert_eq!(tool_spawns(&by_path).len(), 1);
    let shell = format!("{spawn}(\"bash\").arg(\"-c\").arg(\"{fsck} -n x.img\");");
    assert_eq!(shell_spawns(&shell).len(), 1);
    let macos = format!("{spawn}(\"/sbin/fsck_exfat\")");
    assert!(
        tool_spawns(&macos).is_empty(),
        "macOS's checker is not a Linux tool"
    );
    let harness = [
        "let vm = \"../fs-linux",
        "-test-harness/scripts/vm",
        ".sh\";",
    ]
    .concat();
    assert_eq!(harness_spawns(&harness).len(), 1);
    let skip = ["println!(\"", "SKIP", ": no VM\");"].concat();
    assert_eq!(announced_skips(&skip).len(), 1);
}
