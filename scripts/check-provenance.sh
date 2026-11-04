#!/usr/bin/env bash
set -euo pipefail
if rg -n 'github\.com/(torvalds/linux|exfatprogs/exfatprogs)|fs/exfat/|GPL-[0-9]|LGPL-[0-9]|AGPL-[0-9]' src tests fuzz README.md PROVENANCE.md; then
    echo 'provenance: prohibited source reference or license' >&2
    exit 1
fi
echo 'provenance: all checks passed'
