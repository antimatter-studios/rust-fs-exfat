#!/usr/bin/env bash
set -euo pipefail
[ "$#" -eq 2 ] || { echo 'capi: two generated oracle images are required' >&2; exit 1; }
command -v cc >/dev/null || { echo 'capi: a C compiler is required by chore test:oracle-macos' >&2; exit 1; }
cargo build --locked --features cli --lib
target_directory="${CARGO_TARGET_DIR:-target}"
cc -std=c11 -Wall -Wextra -Werror -Iinclude tests/capi_smoke.c "$target_directory/debug/libfs_exfat.a" -liconv -lSystem -lm -o "$target_directory/capi-smoke"
"$target_directory/capi-smoke" "$1"
"$target_directory/capi-smoke" "$2"
mkdir -p tmp
formatted="$(mktemp "$PWD/tmp/capi-mkfs.XXXXXX")"
/usr/bin/truncate -s 16m "$formatted"
"$target_directory/capi-smoke" --mkfs "$formatted"
rm "$formatted"
