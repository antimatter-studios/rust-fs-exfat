#!/usr/bin/env bash
#
# ci-install-chore.sh — install the pinned chore on a CI runner.
#
# Every CI job runs chore tasks, so chore comes first, pinned by
# CHORE_VERSION (set in the workflow) and checked against the release's own
# checksums. Linux on x86_64 (unit, semver, cli ...) and aarch64
# (test-arm64), and macOS on arm64 (oracle-macos).
set -euo pipefail

: "${CHORE_VERSION:?set CHORE_VERSION, e.g. 0.19.0}"
case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) echo "ci-install-chore: no chore build for $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64) arch=x86_64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) echo "ci-install-chore: no chore build for $(uname -m)" >&2; exit 1 ;;
esac
tarball="chore-${CHORE_VERSION}-${os}-${arch}.tar.gz"
base="https://github.com/antimatter-studios/chore/releases/download/v${CHORE_VERSION}"
dir="${RUNNER_TEMP:-$(mktemp -d)}/chore"
mkdir -p "$dir"
# GitHub's release download answers an occasional HTTP 500, and one such
# answer would fail the whole job before anything under test ran. Both
# downloads retry on any error; the checksum check below still guards what
# was fetched.
curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 -o "$dir/$tarball" "$base/$tarball"
curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 -o "$dir/checksums.txt" "$base/checksums.txt"
(cd "$dir" && grep " ${tarball}\$" checksums.txt | shasum -a 256 -c -)
tar -xzf "$dir/$tarball" -C "$dir"
bin="$(find "$dir" -type f -name chore -perm -u+x | head -1)"
[ -n "$bin" ] || { echo "ci-install-chore: no chore binary in $tarball" >&2; exit 1; }
mkdir -p "$HOME/.local/bin"
cp "$bin" "$HOME/.local/bin/chore"
chmod 0755 "$HOME/.local/bin/chore"
[ -z "${GITHUB_PATH:-}" ] || echo "$HOME/.local/bin" >> "$GITHUB_PATH"
"$HOME/.local/bin/chore" --version | head -1
