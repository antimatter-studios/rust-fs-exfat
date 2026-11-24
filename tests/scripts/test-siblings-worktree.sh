#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! sed -n '/  siblings:/,/^  tools:/p' "$REPO/chores.yml" | rg -q 'siblings_root=.*cd \.\. && pwd'; then
    echo "siblings: lookup must follow the worktree's sibling links" >&2
    exit 1
fi
if sed -n '/  siblings:/,/^  tools:/p' "$REPO/chores.yml" | rg -q 'git rev-parse --path-format=absolute --git-common-dir|this checkout is not beside'; then
    echo "siblings: lookup must not require a worktree beside the main checkout" >&2
    exit 1
fi

echo "siblings: worktree-relative sibling links are supported"
