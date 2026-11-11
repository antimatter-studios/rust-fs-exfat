#!/usr/bin/env bash
#
# vm-setup.sh — the fs-linux-test-harness [setup] script. Runs as root
# INSIDE the VM, re-applied by the harness whenever this file changes
# (and, in a guest shared with other consumers, whenever this consumer
# joins a new boot).
#
# THE GUEST IS WHERE THE LINUX ORACLES LIVE. Not the host: exfatprogs on
# a workstation is whatever that machine has, a different version per
# developer, and a Mac has none. One Debian guest, one version, the same
# answers for everyone.
#
#   exfatprogs  mkfs.exfat, fsck.exfat, dump.exfat, exfatlabel: the
#               fixture builder's formatter and the tool oracle
#   python3     the fixture builder's damage script and kernel listing
#   util-linux  losetup and mount: the loop mounts through which the
#               kernel's exfat driver writes the fixtures and reads them
#               back for the kernel oracle, here and nowhere else
#   exfat       the kernel module. Loaded here so that a guest without it
#               fails setup, naming it, rather than failing a mount later
#   relan/exfat A THIRD IMPLEMENTATION OF EXFAT, built here from source at a
#               pinned commit into /opt/relan-exfat: mount.exfat-fuse (a
#               FUSE driver) and exfatfsck. It shares no code with this crate
#               or with the kernel, so where it disagrees with both, one of
#               the three has the format wrong (tests/third.rs). Its own
#               prefix keeps its exfatlabel and mkfs apart from exfatprogs'.
#               Built against libfuse 2; as root it mounts without
#               fusermount, so no fuse package is installed beside a fuse3
#               another consumer of a shared guest may have
#
# exfatprogs, the kernel driver and relan/exfat are GPL. They run here as
# oracles, never linked and never read: AGENTS.md and PROVENANCE.md. relan's
# source is fetched and compiled here, in the guest, and nothing in this
# repository reads it.
#
# AND WHAT A RUST BUILD NEEDS FROM THE DISTRIBUTION (curl, gcc, libc6-dev,
# pkg-config), for running the suite in here on a host that is not Linux.
# Not the toolchain itself: that is rust-fs-core's
# scripts/guest-rust-toolchain.sh, run by the in-guest suite once the core
# sibling is staged.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

apt-get update -qq
apt-get install -y -qq exfatprogs util-linux coreutils python3 curl gcc libc6-dev pkg-config \
    git make autoconf automake libfuse-dev >/dev/null
modprobe loop
modprobe fuse
modprobe exfat || {
    echo "vm-setup: the guest kernel $(uname -r) has no exfat module" >&2
    exit 1
}
grep -qw exfat /proc/filesystems || {
    echo "vm-setup: exfat is loaded but not listed in /proc/filesystems" >&2
    exit 1
}

# 1.2.0 is Debian 12's exfatprogs, the version every fixture and oracle
# expectation here was written against. An older one lacks options the
# fixture builder passes.
#
# `-V` prints the version and EXITS NON-ZERO in 1.2.0 (mkfs.exfat 1,
# fsck.exfat 16), so its status is not the verdict here: the printed
# version is, and pipefail must not see the tool's own status.
version="$( { mkfs.exfat -V 2>&1 || true; } | sed -n 's/^exfatprogs version : \([0-9][0-9.]*\).*/\1/p' | head -1)"
if [ -z "$version" ] ||
    [ "$(printf '%s\n%s\n' 1.2.0 "$version" | sort -V | head -1)" != 1.2.0 ]; then
    echo "vm-setup: exfatprogs ${version:-of unknown version} is older than 1.2.0" >&2
    exit 1
fi
for tool in mkfs.exfat fsck.exfat dump.exfat exfatlabel python3; do
    command -v "$tool" >/dev/null || { echo "vm-setup: exfatprogs installed no $tool" >&2; exit 1; }
done

# relan/exfat, at the pinned commit. A commit, not a branch: an oracle that
# moves on its own gives verdicts nobody can compare with yesterday's. Bump
# it deliberately. 1264da3 is master as of 2025-12-21, upstream's newest
# (v1.4.0 and later). tests/walk_support/mod.rs holds the same string and
# refuses a guest built from another, so the two cannot drift.
#
# Idempotent by the stamp, written last, which holds the commit built. A
# shallow fetch OF THE COMMIT, which `git clone --branch` cannot express.
RELAN_PIN=1264da3d59840434c657e2199d9c8248355940a6
RELAN_REPO=https://github.com/relan/exfat.git
RELAN_SRC=/var/lib/rust-fs-exfat-relan
RELAN_PREFIX=/opt/relan-exfat
relan_stamp="$RELAN_PREFIX/relan-exfat.pin"
if [ "$(cat "$relan_stamp" 2>/dev/null || true)" != "$RELAN_PIN" ]; then
    echo "vm-setup: building relan/exfat $RELAN_PIN"
    rm -rf "$RELAN_SRC" "$RELAN_PREFIX"
    git init --quiet "$RELAN_SRC"
    git -C "$RELAN_SRC" remote add origin "$RELAN_REPO"
    git -C "$RELAN_SRC" fetch --quiet --depth 1 origin "$RELAN_PIN"
    git -C "$RELAN_SRC" checkout --quiet FETCH_HEAD
    [ "$(git -C "$RELAN_SRC" rev-parse HEAD)" = "$RELAN_PIN" ] || {
        echo "vm-setup: relan/exfat is not at $RELAN_PIN" >&2
        exit 1
    }
    (cd "$RELAN_SRC" && autoreconf --install >/dev/null 2>&1 &&
        ./configure --prefix="$RELAN_PREFIX" >/dev/null &&
        make -j"$(nproc)" >/dev/null && make install >/dev/null)
    printf '%s\n' "$RELAN_PIN" > "$relan_stamp"
fi
for tool in mount.exfat-fuse exfatfsck; do
    [ -x "$RELAN_PREFIX/sbin/$tool" ] || {
        echo "vm-setup: relan/exfat built no $RELAN_PREFIX/sbin/$tool" >&2
        exit 1
    }
done

echo "vm-setup: exfatprogs $version, kernel $(uname -r) with exfat, relan/exfat $(cat "$relan_stamp")"
