#!/usr/bin/env bash
#
# tools.sh — what the HOST needs (`chore tools`).
#
#   tools.sh           install what is missing, then verify, VM included
#   tools.sh --check   verify the host's own tools only; exit 1 naming
#                      what is missing
#
# THE LINUX ORACLES ARE NOT HERE, AND THAT IS THE POINT. exfatprogs —
# mkfs.exfat, fsck.exfat, dump.exfat — and the kernel's exfat driver run
# inside the fs-linux-test-harness VM, provisioned by scripts/vm-setup.sh,
# and nowhere else. A workstation installs none of it, so no oracle answers
# differently depending on who asked. macOS's own exFAT tools are part of
# macOS and need no install; `chore test:oracle-macos` checks for them.
#
# What the host does need:
#
#   ripgrep      chore lint's source checks search the tree with it; a check
#                written as `if rg ...; then fail` reports PASS when rg is
#                missing, having matched nothing
#   python3      chore lint's image-safety check, the macOS oracle's
#                receipts, and rust-fs-core's family checks, which also need
#                PyYAML
#   the VM       Vagrant, QEMU and KVM/HVF: checked by the harness itself
#                (../fs-linux-test-harness/scripts/host-tools.sh, also
#                `chore vm:host:check`), which knows what each platform needs
#
# `--check` LEAVES THE VM OUT. It is the early gate other tasks run so that
# a missing tool fails once, and it is what a runner with no KVM can run.
# Whether the VM works is answered by booting it.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
HARNESS_TOOLS="$REPO/../fs-linux-test-harness/scripts/host-tools.sh"

MODE=install
[ "${1:-}" = "--check" ] && MODE=check

# tool:debian-package:homebrew-formula
TOOLS="rg:ripgrep:ripgrep python3:python3:python"

missing() {
    local entry out=""
    for entry in $TOOLS; do
        command -v "${entry%%:*}" >/dev/null 2>&1 || out="$out $entry"
    done
    echo "${out# }"
}

names() {
    local entry
    for entry in "$@"; do printf '%s ' "${entry%%:*}"; done
}

packages() {
    local field="$1" entry
    shift
    for entry in "$@"; do echo "$entry" | cut -d: -f"$field"; done | sort -u | tr '\n' ' '
}

install_linux() {
    local sudo="" pkgs
    # shellcheck disable=SC2046  # one word per package
    pkgs="$(packages 2 $(missing))"
    [ "$(id -u)" -eq 0 ] || sudo="sudo"
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "tools: no apt-get on this Linux host; install with its package manager: $pkgs" >&2
        exit 1
    fi
    echo "tools: installing with apt-get: $pkgs"
    # shellcheck disable=SC2086  # the package list splits into words
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $pkgs >/dev/null ||
        { $sudo apt-get update -qq && $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $pkgs >/dev/null; }
}

gap="$(missing)"
if [ -n "$gap" ] && [ "$MODE" = install ]; then
    case "$(uname -s)" in
        Linux) install_linux ;;
        Darwin)
            # shellcheck disable=SC2086
            echo "tools: missing on this Mac: $(names $gap)" >&2
            # shellcheck disable=SC2086
            echo "       brew install $(packages 3 $gap)" >&2
            exit 1
            ;;
        *) echo "tools: unsupported host $(uname -s)" >&2; exit 1 ;;
    esac
    gap="$(missing)"
fi

if [ -n "$gap" ]; then
    # shellcheck disable=SC2086
    echo "tools: missing: $(names $gap)— run 'chore tools'" >&2
    exit 1
fi

if ! python3 -c 'import yaml' 2>/dev/null; then
    echo "tools: python3 has no PyYAML, which rust-fs-core's family checks import." >&2
    echo "       pip install PyYAML (CI installs 6.0.3 into a venv)" >&2
    exit 1
fi

printf '  %-9s %s\n' rg "$(command -v rg)" python3 "$(command -v python3)"
echo "tools: $(rg --version | head -1), $(python3 --version) — the host's own tools are present"
echo "tools: exfatprogs and the kernel oracle live in the harness VM (scripts/vm-setup.sh), not here."

[ "$MODE" = check ] && exit 0

if [ ! -x "$HARNESS_TOOLS" ]; then
    echo "tools: the fs-linux-test-harness sibling is not checked out." >&2
    echo "       Run 'chore siblings'. exfatprogs, the fixtures and the kernel" >&2
    echo "       oracle all run in its VM." >&2
    exit 1
fi
"$HARNESS_TOOLS"
