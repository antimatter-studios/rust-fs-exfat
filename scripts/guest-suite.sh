#!/usr/bin/env bash
#
# guest-suite.sh [cargo test args...] — EVERY LINUX TIER, INSIDE THE VM.
#
# The fs-linux-test-harness [test] guest_command: `chore test:vm` (and
# `chore test` on a host that is not Linux) boots the VM and runs this from
# /repo, where the harness mounts this repository.
#
# WHY IT EXISTS. We run the Linux tests on Linux. On a Linux host that is
# the host itself and this is not used. On a Mac there is no exfat kernel
# driver, no exfatprogs and no loop mount, so the suite runs in here, from
# the same sources, with the same pinned toolchain: unit, images, the
# exfatprogs oracle, the kernel oracle and relan/exfat. Inside the guest the
# harness sets FLTH_GUEST=1, and tests/guest_support runs each guest script
# in a local shell rather than over SSH.
#
# THE TARGETS ARE EVERY tests/*.rs, derived rather than listed, so a test
# added later is run here with no edit: the library, every top-level test
# file, and the doctests. tests/oracle/*.rs are the macOS oracle's
# (`chore test:oracle-macos`, on a Mac host) and are not top-level files.
#
# The toolchain and the build directory live on the guest's own disk
# (/var/lib, /var/cache), so the first run pays a full build and later runs
# on the same guest are incremental.
set -euo pipefail

[ "${FLTH_GUEST:-}" = 1 ] ||
    { echo "guest-suite.sh runs INSIDE the harness VM ('chore test:vm')." >&2; exit 1; }

RUST_ROOT=/var/lib/rust-fs-exfat-rust
export RUSTUP_HOME="$RUST_ROOT/rustup"
export CARGO_HOME="$RUST_ROOT/cargo"
export CARGO_TARGET_DIR=/var/cache/rust-fs-exfat-target
export PATH="$CARGO_HOME/bin:$PATH"

# THE CORE SIBLING. Its scripts (the toolchain install) are reached as
# ../rust-fs-core, and the guest is given this repository, not the directory
# that holds it. `chore test:vm` stages the pinned checkout on the share,
# and this links it where ../rust-fs-core resolves from /repo.
staged=/share/siblings/rust-fs-core
[ -d "$staged" ] || {
    echo "guest-suite.sh: rust-fs-core is not staged on the share; 'chore test:vm' does that." >&2
    exit 1
}
[ -L /rust-fs-core ] || ln -sfn "$staged" /rust-fs-core

cd /repo
bash ../rust-fs-core/scripts/guest-rust-toolchain.sh
command -v cargo >/dev/null ||
    { echo "guest-suite.sh: no cargo in the guest after the toolchain install." >&2; exit 1; }

targets=(--lib)
for file in tests/*.rs; do
    targets+=(--test "$(basename "$file" .rs)")
done

echo "== in-guest suite: $(uname -srm), $(cargo --version), ${#targets[@]} targets"
started=$(date +%s)
# SCRATCH ON TMPFS. The tests make their images under tmp/, and one of them
# (tests/check/scale.rs) a sparse 2 TiB format-maximum volume. The guest's
# root ext4 has no huge_file feature, which caps a file at 2 TiB, so it
# refuses that with EFBIG ("File too large"); tmpfs has no such cap and
# stores only the pages written. Mounted in this run's own namespace.
mkdir -p tmp
mountpoint -q /repo/tmp || mount -t tmpfs -o mode=1777 rust-fs-exfat-tmp /repo/tmp
export TMPDIR=/repo/tmp
cargo test --locked --profile test-opt --no-fail-fast --features cli,test-support "${targets[@]}" "$@"
cargo test --locked --profile test-opt --all-features --doc
echo "== in-guest suite: $(( $(date +%s) - started ))s"
