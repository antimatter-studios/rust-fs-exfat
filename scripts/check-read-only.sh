#!/usr/bin/env bash
set -euo pipefail
# The driver writes (fs_exfat::write), but only there: the strict reader, the
# salvage adapter, the checker and every other module take BlockRead and must
# not reach a writable device. The C ABI holds the writable mounts, because
# callback devices need its unsafe boundary.
if rg -n 'BlockDevice|write_at|is_writable|OpenOptions|std::fs::write|open_rw|open_best_effort|OwnedRwSlice|CallbackDevice' src --glob '*.rs' --glob '!write.rs' --glob '!write_*.rs' --glob '!capi.rs'; then
    echo 'read-only: writable surface outside the write module and the C ABI' >&2
    exit 1
fi
if rg -n '\bunsafe\b|#\[(allow|expect)\([^]]*unsafe_code' src --glob '*.rs' --glob '!capi.rs' --glob '!lib.rs'; then
    echo 'read-only: unsafe code is confined to the C ABI boundary' >&2
    exit 1
fi
if rg -n '\bunsafe\b' src/lib.rs; then
    echo 'read-only: unsafe code is forbidden in the library entry point' >&2
    exit 1
fi
grep -Fxq '#![deny(unsafe_code)]' src/lib.rs
[ "$(grep -Fc '#[allow(unsafe_code)]' src/lib.rs)" -eq 1 ]
grep -A1 -Fx '#[allow(unsafe_code)]' src/lib.rs | grep -Fxq 'pub mod capi;'
if rg -n '^write[[:space:]]*=' Cargo.toml; then
    echo 'read-only: writing is not an optional feature; the read-only APIs are read-only by type' >&2
    exit 1
fi
grep -q 'ReadOnlyDevice<Device>' src/device.rs
grep -Fq 'pub struct Volume<Device: BlockRead>' src/volume.rs
grep -Fq 'pub fn probe(source: &dyn BlockRead)' src/salvage_exfat.rs
echo 'read-only: all checks passed'
