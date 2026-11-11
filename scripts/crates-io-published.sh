#!/usr/bin/env bash
# Is this crate on crates.io? Prints `published=true` or `published=false`
# on stdout -- the shape of a GitHub step output -- and exits 0 only when
# crates.io actually answered.
#
#   200  published          check:semver compares against the newest release
#   404  not published      check:semver compares against main
#   anything else           not an answer: retried, then exit 1
#
# The status is read from the response, not from curl's exit code. `curl -f`
# exits non-zero alike on a 404, a 429, a 5xx, a DNS failure and a timeout,
# and treating all of them as "not published" sent a rate-limited re-run of
# an already-published release on to `cargo publish` in rust-fs-ext4 (its
# #329). Here it would send a published crate's semver check to the wrong
# baseline. A step that fails when crates.io cannot say is a red run that
# names the real cause.
#
#   scripts/crates-io-published.sh                    Cargo.toml's name, any version
#   scripts/crates-io-published.sh <name>             that crate, any version
#   scripts/crates-io-published.sh <name> <version>   that version of it
#
# CRATES_IO_ATTEMPTS (default 4) and CRATES_IO_RETRY_DELAY (seconds before
# the first retry, doubled each time; default 5) bound the retries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case $# in
    0) name="$(grep -m1 '^name = ' "$ROOT/Cargo.toml" | sed 's/name = //; s/"//g')" version="" ;;
    1) name="$1" version="" ;;
    2) name="$1" version="$2" ;;
    *) echo "usage: $0 [<name> [<version>]]" >&2; exit 2 ;;
esac
[ -n "$name" ] || { echo "crates-io-published: no crate name" >&2; exit 2; }

attempts="${CRATES_IO_ATTEMPTS:-4}"
delay="${CRATES_IO_RETRY_DELAY:-5}"
url="https://crates.io/api/v1/crates/$name${version:+/$version}"
what="$name${version:+ $version}"

for ((attempt = 1; ; attempt++)); do
    # No -f: the status is the answer, and -f would fold it into the exit
    # code. A connection that never completes prints 000.
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 30 \
        -H 'User-Agent: rust-fs-exfat-ci (github.com/antimatter-studios/rust-fs-exfat)' "$url")" || true
    case "$code" in
        200)
            echo "$what is on crates.io." >&2
            echo "published=true"
            exit 0 ;;
        404)
            echo "$what is not on crates.io." >&2
            echo "published=false"
            exit 0 ;;
    esac
    if [ "$attempt" -ge "$attempts" ]; then
        echo "crates-io-published: $url answered HTTP ${code:-000} on each of $attempts attempts;" \
             "cannot tell whether $what is published" >&2
        exit 1
    fi
    echo "crates-io-published: HTTP ${code:-000} from $url; retrying in ${delay}s" >&2
    sleep "$delay"
    delay=$((delay * 2))
done
