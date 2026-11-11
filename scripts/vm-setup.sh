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
#
# exfatprogs and the kernel driver are GPL. They run here as oracles,
# never linked and never read: AGENTS.md and PROVENANCE.md.
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
    git make >/dev/null
modprobe loop
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

echo "vm-setup: exfatprogs $version, kernel $(uname -r) with exfat"
