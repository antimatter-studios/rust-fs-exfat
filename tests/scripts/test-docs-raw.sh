#!/usr/bin/env bash
# The documents do not list a raw device with 4096-byte sectors as unreadable.
#
# #56 made the reader adapt to the device's sector alignment, and
# `chore test:oracle-macos` compares raw and block nodes with the macOS driver
# at both sector sizes. The README and a feature row kept the old limit (#251).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STALE='raw devices? with 4096-byte sectors (cannot be read|(is |are )?unreadable)'

# The documents wrap their lines, so each is matched as one line.
stale() { tr -s '[:space:]' ' ' | grep -Eiq "$STALE"; }

# The pattern recognises both wordings it was written against, wrapped or not.
for sample in 'A raw device with 4096-byte sectors cannot be read; its block node can' \
  $'raw devices with 4096-byte\nsectors unreadable (#20)'; do
  stale <<<"$sample" || { echo "FAIL: the pattern misses: $sample" >&2; exit 1; }
done

fails=0
for document in "$REPO"/README.md "$REPO"/docs/*.md "$REPO"/features.d/*.md; do
  [[ -f "$document" ]] || { echo "FAIL: no document at $document" >&2; exit 1; }
  if stale <"$document"; then
    echo "FAIL: ${document#"$REPO"/} still says a 4096-byte-sector raw device cannot be read (#56 fixed it)" >&2
    fails=$((fails + 1))
  fi
done

# The README's layers are the ones the crate has: rust-fs-core's shared traits,
# implemented by `layers::ExfatVolume`. It names no consuming application and no
# revision one of them pins.
readme="$REPO/README.md"
section="$(awk '/^## /{inside = ($0 == "## API layers")} inside' "$readme")"
for name in fs_core::layers layers::ExfatVolume open_for_salvage; do
  grep -Fq "$name" <<<"$section" ||
    { echo "FAIL: README.md \"API layers\" does not name $name" >&2; fails=$((fails + 1)); }
done
if grep -Fq 'Captain Salvage' "$readme"; then
  echo "FAIL: README.md names a consuming application" >&2
  fails=$((fails + 1))
fi

((fails == 0)) || exit 1
echo "test-docs-raw: ok"
