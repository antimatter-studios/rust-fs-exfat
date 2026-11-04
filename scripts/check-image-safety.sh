#!/usr/bin/env bash
set -euo pipefail
source_file="${1:-scripts/test-oracle.sh}"
[ -f "$source_file" ] && [ ! -L "$source_file" ] || { echo 'image-safety: a regular source file is required' >&2; exit 1; }
command -v grep >/dev/null || { echo 'image-safety: grep is required by chore lint' >&2; exit 1; }
status=0
grep -nE '(^|[^[:alnum:]_])sync([^[:alnum:]_]|$)' "$source_file" || status=$?
if [ "$status" -eq 0 ]; then
    echo 'image-safety: global filesystem flushes are forbidden; detach only the image device from its own receipt' >&2
    exit 1
fi
[ "$status" -eq 1 ] || { echo 'image-safety: source scan failed' >&2; exit "$status"; }
echo 'image-safety: no global filesystem flush'
