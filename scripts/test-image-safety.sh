#!/usr/bin/env bash
set -euo pipefail
mkdir -p tmp
directory="$(mktemp -d "$PWD/tmp/image-safety.XXXXXX")"
trap 'rm -r "$directory"' EXIT
printf 'os.fsync(output.fileno())\n' > "$directory/allowed.py"
bash scripts/check-image-safety.sh "$directory/allowed.py"
printf 'os.sync()\n' > "$directory/global.py"
printf 'sync\n' > "$directory/global.sh"
printf '/bin/sync\n' > "$directory/absolute.sh"
for source_file in "$directory/global.py" "$directory/global.sh" "$directory/absolute.sh"; do
    if bash scripts/check-image-safety.sh "$source_file" > "$directory/rejected.log" 2>&1; then
        echo 'image-safety: a global flush was incorrectly accepted' >&2
        exit 1
    fi
done
bash scripts/check-image-safety.sh
echo 'image-safety: positive and negative guard checks passed'
