#!/usr/bin/env bash
set -euo pipefail
# A check that cannot search must not pass: `if rg ...` would read a missing rg
# (status 127) or a failed search (status 2) as "no match" (#138).
command -v rg >/dev/null || {
    echo 'provenance: ripgrep (rg) is missing, so nothing was searched; `chore tools` installs it' >&2
    exit 1
}
# refuse <message> <rg arguments...>: fail with <message> when rg matches, and
# fail when rg could not search.
refuse() {
    local message="$1" status=0
    shift
    rg -n "$@" || status=$?
    case "$status" in
        0) echo "provenance: $message" >&2; exit 1 ;;
        1) ;;
        *) echo "provenance: rg could not search (status $status), so nothing was checked" >&2; exit 1 ;;
    esac
}
refuse 'prohibited source reference or license' \
    'github\.com/(torvalds/linux|exfatprogs/exfatprogs)|fs/exfat/|GPL-[0-9]|LGPL-[0-9]|AGPL-[0-9]' src tests fuzz README.md PROVENANCE.md
echo 'provenance: all checks passed'
