#!/usr/bin/env bash
#
# ci-install-chore.sh — install the pinned chore release on a CI runner and
# put it on PATH. Every CI job runs a chore task, so every job runs this.
# The archive is checked against the release's own checksums.
set -euo pipefail

VERSION=0.19.0
dir="${RUNNER_TEMP:?ci-install-chore.sh runs on a GitHub runner}/chore"
mkdir -p "$dir"
case "${RUNNER_OS:-}-${RUNNER_ARCH:-}" in
    Linux-X64) archive="chore-$VERSION-linux-x86_64.tar.gz" ;;
    Linux-ARM64) archive="chore-$VERSION-linux-arm64.tar.gz" ;;
    macOS-ARM64) archive="chore-$VERSION-darwin-arm64.tar.gz" ;;
    *) echo "ci-install-chore: unsupported runner ${RUNNER_OS:-}-${RUNNER_ARCH:-}" >&2; exit 1 ;;
esac
gh release download "v$VERSION" -R antimatter-studios/chore -p "$archive" -p checksums.txt -D "$dir" --clobber
cd "$dir"
grep " $archive\$" checksums.txt | shasum -a 256 -c -
tar -xzf "$archive"
echo "$dir" >> "${GITHUB_PATH:?}"
