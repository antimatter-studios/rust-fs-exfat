# tests/cli/lib.sh — what every tests/cli/test-*.sh sources.
#
# The house style: `ok`/`fail`, a `fails` counter, `set -uo pipefail` (not
# -e, so later checks still run after one fails), a sandbox under the
# repository's tmp/, and `finish` last, which prints the count
# ../rust-fs-core/scripts/test-floor.sh reads and the trailing
# `<name>: all checks passed` line scripts/test-cli.sh requires.
#
# The tools are whatever PATH finds: scripts/test-cli.sh has already made
# sure they are ours before any file runs.
set -uo pipefail

NAME="$(basename "$0" .sh)"
REPO="$(cd "$(dirname "$0")/../.." && pwd -P)"
CRATE="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -n 1)"

passed=0
fails=0
ok() { passed=$((passed + 1)); }
fail() {
    echo "FAIL  $NAME: $*" >&2
    fails=$((fails + 1))
}

# check DESCRIPTION COMMAND...: ok if COMMAND succeeds, else fail naming it.
check() {
    local what="$1"
    shift
    if "$@"; then ok; else fail "$what"; fi
}

# jq_check DESCRIPTION FILTER FILE: ok if jq -e FILTER holds for FILE.
jq_check() {
    local what="$1" filter="$2" file="$3"
    if jq -e "$filter" "$file" >/dev/null 2>&1; then
        ok
    else
        fail "$what: jq -e '$filter' is false for: $(head -c 2000 "$file")"
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

mkdir -p "$REPO/tmp"
SANDBOX="$(mktemp -d "$REPO/tmp/cli-$NAME.XXXXXX")"
trap 'rm -rf "$SANDBOX"' EXIT HUP INT TERM

# sample_image: a newly made copy of the synthetic volume in the sandbox,
# printed as its path. tests/cli/sample.img.gz holds it compressed, and
# sample.img.sha256 its digest, which tests/cli.rs holds equal to the digest
# of the volume tests/interface_support builds -- so these files test the
# same bytes the cargo tests do, and a stale copy fails here or there.
sample_image() {
    local image="$SANDBOX/sample.img"
    gzip -dc "$REPO/tests/cli/sample.img.gz" >"$image" || {
        echo "FAIL  $NAME: cannot decompress tests/cli/sample.img.gz" >&2
        exit 1
    }
    [ "$(sha256_of "$image")" = "$(cat "$REPO/tests/cli/sample.img.sha256")" ] || {
        echo "FAIL  $NAME: tests/cli/sample.img.gz is not the volume sample.img.sha256 names" >&2
        exit 1
    }
    printf '%s\n' "$image"
}

finish() {
    if [ "$fails" -gt 0 ]; then
        echo "test result: FAILED. $passed passed; $fails failed"
        exit 1
    fi
    echo "test result: ok. $passed passed; 0 failed"
    echo "$NAME: all checks passed"
    exit 0
}
