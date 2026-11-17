#!/usr/bin/env bash
# test.sh [cargo test args...] — run cargo test as a harness session.
#
# THE RUN OWNS ITS SESSION, NOT THE SHARED GUEST. A Linux-oracle test joins
# the harness guest from inside its own process (tests/guest_support/mod.rs).
# `vm.sh session` runs cargo inside a harness session, so only its own run
# ends when cargo ends, passed, failed or killed. The guest stays up for the
# next run and other consumers remain undisturbed. In the guest, and on a
# host that cannot run the VM, the harness runs cargo as it is.
#
# Scratch stays inside the repository (tmp/), visible under /repo in the
# run's mount namespace.
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
