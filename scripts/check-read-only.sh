#!/usr/bin/env bash
set -euo pipefail
if rg -n 'BlockDevice|write_at|is_writable|OpenOptions|std::fs::write|unsafe' src --glob '*.rs' | grep -v '#!\[forbid(unsafe_code)\]'; then
    echo 'read-only: writable or unsafe surface in library source' >&2
    exit 1
fi
if rg -n '^write[[:space:]]*=' Cargo.toml; then
    echo 'read-only: write feature is forbidden' >&2
    exit 1
fi
grep -q 'ReadOnlyDevice<Device>' src/device.rs
echo 'read-only: all checks passed'
