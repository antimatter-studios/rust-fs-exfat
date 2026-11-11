#!/usr/bin/env bash
# test.sh [cargo test args...] — run cargo test as a harness session.
#
# THE RUN OWNS THE VM ITS TESTS BOOT. A Linux-oracle test brings the harness
# VM up from inside its own process (tests/guest_support/mod.rs), and that
# boot takes the machine-wide slot. `vm.sh session` runs cargo inside a
# harness session, so the VM comes down and the slot is released when the
# run ends, passed, failed or killed; a VM held with `chore vm:up`, or a
# shared guest another suite is using, is left up. In the guest, and on a
# host that cannot run the VM, the harness runs cargo as it is.
#
# Scratch stays inside the repository (tmp/), the only host directory the
# guest can see.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
HARNESS_VM="$REPO/../fs-linux-test-harness/scripts/vm.sh"
[[ -x "$HARNESS_VM" ]] || {
    echo "test.sh: the fs-linux-test-harness sibling is not checked out; run 'chore siblings'." >&2
    exit 1
}
mkdir -p "$REPO/tmp"
export TMPDIR="$REPO/tmp"
cd "$REPO"
exec "$HARNESS_VM" session cargo test "$@"
