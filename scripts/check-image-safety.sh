#!/usr/bin/env bash
set -euo pipefail
source_file="${1:-scripts/test-oracle.sh}"
[ -f "$source_file" ] && [ ! -L "$source_file" ] || { echo 'image-safety: a regular source file is required' >&2; exit 1; }
command -v python3 >/dev/null || { echo 'image-safety: python3 is required by chore lint' >&2; exit 1; }
script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$script_directory/check-image-safety.py" "$source_file"
python3 "$script_directory/check-image-safety.py" "$script_directory/../tests/oracle/scale.rs"
python3 "$script_directory/check-image-safety.py" "$script_directory/../tests/scale_support/mod.rs"
