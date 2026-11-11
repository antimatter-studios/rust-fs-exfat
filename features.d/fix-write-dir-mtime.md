## Writing

| Directory times follow their entries | Experimental; `create`, `mkdir`, `write_file` when it creates, `unlink`, `rmdir` and `rename` set LastModified and LastAccessed on every directory whose entries they change, in the same phase as the entry set, and leave Create and every other directory alone; on the macOS reference volumes the read-only macOS driver shows the same directory times as this crate's reader after a scripted mix, at 512 and 4096 bytes per sector | Unreleased (#151) | #151 | `tests/write_rename.rs`, `tests/oracle/write.rs` |
