#!/usr/bin/env bash
set -euo pipefail
if rg -n 'BlockDevice|write_at|is_writable|OpenOptions|std::fs::write|open_rw|open_best_effort|OwnedRwSlice|CallbackDevice' src --glob '*.rs'; then
    echo 'read-only: writable surface in library source' >&2
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
    echo 'read-only: write feature is forbidden' >&2
    exit 1
fi
grep -q 'ReadOnlyDevice<Device>' src/device.rs
echo 'read-only: all checks passed'
