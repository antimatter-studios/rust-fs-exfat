#!/usr/bin/env bash
# The source checks fail when their search could not run.
#
# check-provenance.sh and check-read-only.sh search the tree with ripgrep and
# fail on a match. Written as `if rg ...; then fail; fi`, a missing `rg`
# (status 127) or a search that errors (status 2) reads as "no match", and the
# check passes having searched nothing -- which is what CI did on runners
# without ripgrep (#138). Each check must refuse a planted match, refuse to pass
# without rg, and refuse to pass when rg cannot search.
#
#   bash tests/scripts/test-source-checks.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail() { echo "FAIL  $*" >&2; exit 1; }

command -v rg >/dev/null ||
    fail "ripgrep (rg) is missing, so the source checks cannot be tested; \`chore tools\` installs it"

SANDBOX="$(mktemp -d)" || exit 1
trap 'rm -rf "$SANDBOX"' EXIT HUP INT TERM

# A PATH with the host's tools but no rg: each tool the checks may call,
# linked into a directory of its own.
mkdir -p "$SANDBOX/no-rg"
for tool in grep head awk sed cat env; do
    ln -s "$(command -v "$tool")" "$SANDBOX/no-rg/$tool"
done

# An rg that cannot search: ripgrep's own status for an error is 2.
mkdir -p "$SANDBOX/broken-rg"
printf '#!/bin/sh\necho "rg: simulated I/O error" >&2\nexit 2\n' > "$SANDBOX/broken-rg/rg"
chmod +x "$SANDBOX/broken-rg/rg"

# check <script> <dir> [PATH]: the script's status in $status, its output in $SANDBOX/log.
check() {
    local script="$1" dir="$2" path="${3:-$PATH}"
    (cd "$dir" && PATH="$path" "$BASH" "$REPO/scripts/$script") >"$SANDBOX/log" 2>&1
    status=$?
}

for script in check-provenance.sh check-read-only.sh; do
    # The tree as it stands passes, so every refusal below is the case's own.
    check "$script" "$REPO"
    [[ $status -eq 0 ]] || fail "$script fails on the tree as it stands: $(cat "$SANDBOX/log")"

    check "$script" "$REPO" "$SANDBOX/no-rg"
    [[ $status -ne 0 ]] || fail "$script passed with no rg on PATH: $(cat "$SANDBOX/log")"
    grep -q 'chore tools' "$SANDBOX/log" ||
        fail "$script without rg does not name \`chore tools\`, which installs it: $(cat "$SANDBOX/log")"

    check "$script" "$REPO" "$SANDBOX/broken-rg:$PATH"
    [[ $status -ne 0 ]] || fail "$script passed although rg could not search: $(cat "$SANDBOX/log")"
done

# A planted match is refused.
plant() {
    rm -rf "$SANDBOX/tree" && mkdir -p "$SANDBOX/tree/src" "$SANDBOX/tree/tests" "$SANDBOX/tree/fuzz"
    : > "$SANDBOX/tree/README.md"
    : > "$SANDBOX/tree/PROVENANCE.md"
    printf '%s\n' "$1" > "$SANDBOX/tree/src/planted.rs"
}
# Split, so that the check does not find it in this file.
plant "// SPDX-License-Identifier: GPL-""2.0"
check check-provenance.sh "$SANDBOX/tree"
[[ $status -eq 1 ]] && grep -q 'prohibited source reference' "$SANDBOX/log" ||
    fail "check-provenance.sh did not refuse a planted GPL licence: status $status, $(cat "$SANDBOX/log")"
plant 'fn f(d: &dyn BlockDevice) {}'
check check-read-only.sh "$SANDBOX/tree"
[[ $status -eq 1 ]] && grep -q 'writable surface' "$SANDBOX/log" ||
    fail "check-read-only.sh did not refuse a planted writable surface: status $status, $(cat "$SANDBOX/log")"

echo "PASS  the source checks refuse a match, a missing rg and a search that could not run"
