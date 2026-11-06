## Reading

| Strict filesystem reader | Experimental; boot regions, system metadata, directories, file reads, timestamps and paths compared with the macOS driver on images macOS formatted; raw devices with 4096-byte sectors unreadable (#20) | Unreleased | #1 | `tests/oracle/directory.rs`, `tests/oracle/file.rs`, `tests/oracle/path.rs` |

## Validation

| macOS reference images and read-only differential | Supported on macOS; new images at 512 and 4096 bytes per sector, fsck_exfat -n agrees, and listings and every file's SHA-256 match the read-only driver; Windows images not covered (#22) | Unreleased | #1 | `scripts/test-oracle.sh`, `tests/oracle/` |
