//! The Linux oracles, run where they live: IN THE HARNESS VM, never on the
//! host.
//!
//! WHY NOT ON THE HOST. exfatprogs on a workstation is whatever that
//! machine has: a distribution build on Linux, a different version per
//! developer, and nothing at all on a Mac. An oracle whose answer depends on
//! which laptop asked is not an oracle. So the tools exist in ONE place, the
//! Debian guest the harness boots and `scripts/vm-setup.sh` provisions, the
//! same guest that built the fixtures. `tests/test_contract.rs` fails the
//! suite if a test reaches a Linux tool any other way than [`guest`].
//!
//! HOW. The first call in a test process brings the VM up (`vm.sh up`,
//! idempotent and quick when it is already running); every call then rides
//! the harness's one shared SSH connection (`vm.sh exec`). The repository is
//! mounted in the guest at `/repo`, so a file under it is passed by its
//! guest path ([`guest_path`]) and nothing is copied across. A tool that
//! works on an image copies it to the guest's own disk first (`/var/tmp`),
//! as the harness README asks: the shares are not a disk.
//!
//! WHEN THE SUITE ITSELF RUNS IN THE GUEST (`chore test:vm`), there is no VM
//! to ask: this is it. The harness sets `FLTH_GUEST=1` in every command it
//! runs there, and the script then runs in a local shell. Same rule, one
//! hop fewer.
//!
//! THE EVIDENCE. Every call prints one `[<tier> vm]` line: what it asked,
//! where it ran and how it exited. `--show-output` keeps those lines in the
//! tier's log, so a green run carries what it checked.
//!
//! NOTHING SKIPS. A missing harness, a VM that will not boot, a tool the
//! guest lacks: each fails the test that needed it, naming the task that
//! fixes it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

/// Linux's limit on one command-line argument on a kernel with 4 KiB pages.
/// A guest script is one argument, so it must be shorter than this.
const MAX_SCRIPT_BYTES: usize = 32 * 4096;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// True when this process is itself running inside the harness guest.
pub fn in_guest() -> bool {
    std::env::var_os("FLTH_GUEST").is_some_and(|value| value == "1")
}

/// The harness's driver script, or a panic naming `chore siblings`.
fn vm_script() -> PathBuf {
    let path = repo().join("../fs-linux-test-harness/scripts/vm.sh");
    assert!(
        path.is_file(),
        "the fs-linux-test-harness sibling is not checked out at {}. `chore siblings` \
         checks it out at the ref chores.yml pins. exfatprogs and the kernel oracle \
         run in its VM and nowhere else, so there is nothing to fall back to.",
        path.display()
    );
    path
}

fn vm(command: &str, argument: &str) -> Output {
    Command::new(vm_script())
        .arg(command)
        .arg(argument)
        .current_dir(repo())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run the harness's vm.sh: {error}"))
}

/// Boot (or join) the VM once per test process.
fn session() {
    static SESSION: OnceLock<()> = OnceLock::new();
    SESSION.get_or_init(|| {
        if in_guest() {
            return;
        }
        let out = vm("up", "");
        assert!(
            out.status.success(),
            "the fs-linux-test-harness VM would not start, so no Linux oracle can run.\n\
             `chore vm:host:check` says what this host is missing; `chore tools` installs \
             it.\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    });
}

/// The path the guest knows a file under this repository by.
#[track_caller]
pub fn guest_path(host: &Path) -> String {
    if in_guest() {
        return host.to_string_lossy().into_owned();
    }
    let relative = host.strip_prefix(repo()).unwrap_or_else(|_| {
        panic!(
            "{} is outside this repository, which is all the guest can see",
            host.display()
        )
    });
    format!("/repo/{}", relative.to_string_lossy())
}

/// Single-quote a word for the guest's shell.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The outcome of one guest script.
pub struct Guest {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Guest {
    #[track_caller]
    pub fn success(self, what: &str) -> Self {
        assert_eq!(
            self.status, 0,
            "{what} failed in the guest with status {}:\n{}{}",
            self.status, self.stdout, self.stderr
        );
        self
    }
}

/// Run a bash script as root in the harness guest, wherever this process is,
/// and print one `[<tier> vm]` evidence line saying what was asked and how it
/// ended. `tier` is `oracle`, `kernel` or `third`.
#[track_caller]
pub fn guest(tier: &str, what: &str, script: &str) -> Guest {
    assert!(
        script.len() < MAX_SCRIPT_BYTES,
        "a {}-byte guest script is one command-line argument, which Linux refuses at \
         {MAX_SCRIPT_BYTES} bytes; pass bulk data in a file under the repository",
        script.len()
    );
    let full = format!("set -euo pipefail\n{script}");
    let out = if in_guest() {
        Command::new("bash")
            .arg("-c")
            .arg(&full)
            .current_dir(repo())
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|error| panic!("cannot run bash in the guest: {error}"))
    } else {
        session();
        vm("exec", &full)
    };
    let status = out.status.code().unwrap_or(-1);
    let place = if in_guest() {
        "in this guest"
    } else {
        "via vm.sh exec"
    };
    println!("[{tier} vm] {what} ({place}): exit {status}");
    Guest {
        status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A shell fragment that copies a fixture to the guest's own disk and leaves
/// its path in `$image`, removed when the script exits. Image tools work on
/// a disk, not on the shares (the harness README, "Where a tool works on an
/// image").
pub fn stage_image(host: &Path) -> String {
    format!(
        "scratch=\"$(mktemp -d /var/tmp/exfat-oracle.XXXXXX)\"\n\
         trap 'rm -rf \"$scratch\"' EXIT\n\
         image=\"$scratch/{name}\"\n\
         cp --sparse=always {source} \"$image\"\n",
        name = host.file_name().unwrap().to_string_lossy(),
        source = quote(&guest_path(host)),
    )
}
