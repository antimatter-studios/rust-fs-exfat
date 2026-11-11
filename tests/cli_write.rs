//! The CLI's writing verbs, on a spec-built volume in a scratch file.

mod write_support;

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use write_support::{format, Layout};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!(
                "cli-write-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("volume.img"), format(&Layout::default()).0).unwrap();
        Self(directory)
    }

    fn image(&self) -> PathBuf {
        self.0.join("volume.img")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(image: &Path, arguments: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .arg("fs")
        .arg(image)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn mkdir_and_write_change_the_image_and_leave_it_clean() {
    let scratch = Scratch::new();
    let image = scratch.image();
    let made = json(&run(&image, &["mkdir", "-p", "/a/b/c"], b""));
    assert_eq!(made["created"], serde_json::json!(["/a", "/a/b", "/a/b/c"]));
    let again = json(&run(&image, &["mkdir", "-p", "/a/b"], b""));
    assert_eq!(again["created"], serde_json::json!([]));
    let refused = run(&image, &["mkdir", "/a"], b"");
    assert!(!refused.status.success());
    let body: Vec<u8> = (0..300_000u32).map(|index| index as u8).collect();
    let written = json(&run(&image, &["write", "/a/b/c/data.bin"], &body));
    assert_eq!(written["written"], 300_000);
    json(&run(
        &image,
        &["write", "--at", "5", "/a/b/c/data.bin"],
        b"XY",
    ));
    let mut expected = body.clone();
    expected[5..7].copy_from_slice(b"XY");
    let cat = run(&image, &["cat", "/a/b/c/data.bin"], b"");
    assert!(cat.status.success());
    assert_eq!(cat.stdout, expected);
    json(&run(&image, &["write", "/a/b/c/data.bin"], b"short"));
    assert_eq!(
        run(&image, &["cat", "/a/b/c/data.bin"], b"").stdout,
        b"short"
    );
    let missing = run(&image, &["write", "--at", "0", "/absent"], b"x");
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["kind"], "not_found");
    json(&run(
        &image,
        &["truncate", "--size", "2", "/a/b/c/data.bin"],
        b"",
    ));
    assert_eq!(run(&image, &["cat", "/a/b/c/data.bin"], b"").stdout, b"sh");
    assert!(!run(&image, &["rmdir", "/a/b/c"], b"").status.success());
    json(&run(&image, &["rm", "/a/b/c/data.bin"], b""));
    let removed = json(&run(&image, &["rmdir", "/a/b/c"], b""));
    assert_eq!(removed["removed"], "/a/b/c");
    json(&run(&image, &["write", "/kept.txt"], b"kept"));
    let bytes = std::fs::read(&image).unwrap();
    assert_eq!(bytes[106] & 2, 0, "the volume is left clean");
    let check = Command::new(env!("CARGO_BIN_EXE_rust-fs-exfat"))
        .args(["fsck", "-n"])
        .arg(&image)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(report["healthy"], true, "{report}");
    assert_eq!(report["unreferenced_clusters"], 0);
}
