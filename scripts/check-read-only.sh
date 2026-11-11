#!/usr/bin/env bash
set -euo pipefail
# A check that cannot search must not pass: `if rg ...` would read a missing rg
# (status 127) or a failed search (status 2) as "no match" (#138).
command -v rg >/dev/null || {
    echo 'read-only: ripgrep (rg) is missing, so nothing was searched; `chore tools` installs it' >&2
    exit 1
}
# refuse <message> <rg arguments...>: fail with <message> when rg matches, and
# fail when rg could not search.
refuse() {
    local message="$1" status=0
    shift
    rg -n "$@" || status=$?
    case "$status" in
        0) echo "read-only: $message" >&2; exit 1 ;;
        1) ;;
        *) echo "read-only: rg could not search (status $status), so nothing was checked" >&2; exit 1 ;;
    esac
}
# The driver writes (fs_exfat::write), but only there: the strict reader, the
# salvage adapter, the checker and every other module take BlockRead and must
# not reach a writable device. The C ABI holds the writable mounts, because
# callback devices need its unsafe boundary.
refuse 'writable surface outside the write module and the C ABI' \
    'BlockDevice|write_at|is_writable|OpenOptions|std::fs::write|open_rw|open_best_effort|OwnedRwSlice|CallbackDevice' src --glob '*.rs' --glob '!write.rs' --glob '!write_*.rs' --glob '!capi.rs'
refuse 'unsafe code is confined to the C ABI boundary' \
    '\bunsafe\b|#\[(allow|expect)\([^]]*unsafe_code' src --glob '*.rs' --glob '!capi.rs' --glob '!lib.rs'
refuse 'unsafe code is forbidden in the library entry point' \
    '\bunsafe\b' src/lib.rs
grep -Fxq '#![deny(unsafe_code)]' src/lib.rs
[ "$(grep -Fc '#[allow(unsafe_code)]' src/lib.rs)" -eq 1 ]
grep -A1 -Fx '#[allow(unsafe_code)]' src/lib.rs | grep -Fxq 'pub mod capi;'
refuse 'writing is not an optional feature; the read-only APIs are read-only by type' \
    '^write[[:space:]]*=' Cargo.toml
grep -q 'ReadOnlyDevice<Device>' src/device.rs
grep -Fq 'pub struct Volume<Device: BlockRead>' src/volume.rs
grep -Fq 'pub fn probe(source: &dyn BlockRead)' src/salvage_exfat.rs
echo 'read-only: all checks passed'
