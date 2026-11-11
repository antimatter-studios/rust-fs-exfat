#!/usr/bin/env bash
# "Is this crate on crates.io?" answers only when crates.io answered.
#
# `curl -sf` exits non-zero alike on 404, 429, a 5xx, a DNS failure and a
# timeout. Read as "not published", any of them would send a published
# crate's semver check to main as its baseline (rust-fs-ext4 met the shape in
# its release, #329). 200 means published, 404 means not, and anything else
# is not an answer: retried, and then the script fails.
#
# This runs scripts/crates-io-published.sh, and check-semver.sh's use of it,
# with `curl` replaced by a stub on PATH.
#
#   bash tests/scripts/test-crates-io-check.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail() { echo "FAIL  $*" >&2; exit 1; }

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT HUP INT TERM

# A curl that answers from $CURL_CODES, one status per call (the last one
# repeats), and behaves as curl does: `-w` prints the code, `-f` exits 22
# on a status of 400 or more, and a code of 000 is a connection that never
# completed -- curl prints 000 for %{http_code} and exits 6. Each call's URL
# goes to $CURL_CALLS.
mkdir -p "$SANDBOX/bin"
cat > "$SANDBOX/bin/curl" <<'STUB'
#!/usr/bin/env bash
out=/dev/stdout fmt="" failflag=0 url=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o|--output) out="$2"; shift ;;
        -w|--write-out) fmt="$2"; shift ;;
        -H|--header|-A|--user-agent|-m|--max-time|--connect-timeout|--retry) shift ;;
        --fail|--fail-with-body) failflag=1 ;;
        --*) ;;
        -*f*) failflag=1 ;;
        -*) ;;
        *) url="$1" ;;
    esac
    shift
done
echo "$url" >> "$CURL_CALLS"
code="$(head -n1 "$CURL_CODES")"
if [ "$(wc -l < "$CURL_CODES")" -gt 1 ]; then
    tail -n +2 "$CURL_CODES" > "$CURL_CODES.next" && mv "$CURL_CODES.next" "$CURL_CODES"
fi
report() { [ -z "$fmt" ] || printf '%s' "${fmt//%\{http_code\}/$code}"; }
if [ "$code" = 000 ]; then report; exit 6; fi
printf '{}' > "$out"
if [ "$failflag" = 1 ] && [ "$code" -ge 400 ]; then report; exit 22; fi
report
exit 0
STUB
chmod +x "$SANDBOX/bin/curl"

# run <codes...> -- <args...>: the script's status in $status, its stdout in $answer.
run() {
    local codes=()
    while [ "$1" != -- ]; do codes+=("$1"); shift; done
    shift
    printf '%s\n' "${codes[@]}" > "$SANDBOX/codes"
    : > "$SANDBOX/calls"
    answer="$(PATH="$SANDBOX/bin:$PATH" CURL_CODES="$SANDBOX/codes" CURL_CALLS="$SANDBOX/calls" \
        CRATES_IO_RETRY_DELAY=0 bash "$REPO/scripts/crates-io-published.sh" "$@" 2>"$SANDBOX/log")"
    status=$?
    calls="$(grep -c . "$SANDBOX/calls")"
}

run 200 --
[[ $status -eq 0 && "$answer" == "published=true" ]] ||
    fail "200 should be published=true, got status $status, '$answer': $(cat "$SANDBOX/log")"
grep -qx 'https://crates.io/api/v1/crates/rust-fs-exfat' "$SANDBOX/calls" ||
    fail "with no arguments the crate is not Cargo.toml's: asked $(cat "$SANDBOX/calls")"

run 404 -- some-crate 1.2.3
[[ $status -eq 0 && "$answer" == "published=false" ]] ||
    fail "404 should be published=false, got status $status, '$answer': $(cat "$SANDBOX/log")"
grep -qx 'https://crates.io/api/v1/crates/some-crate/1.2.3' "$SANDBOX/calls" ||
    fail "a name and version did not ask for that version: asked $(cat "$SANDBOX/calls")"

for code in 503 429 403 000; do
    run "$code" -- some-crate
    [[ $status -ne 0 ]] ||
        fail "a $code from crates.io was read as an answer: status 0, '$answer'"
    [[ -z "$answer" ]] ||
        fail "a $code from crates.io still answered '$answer'"
    [[ $calls -gt 1 ]] ||
        fail "a $code from crates.io was not retried ($calls call)"
done

run 503 200 -- some-crate
[[ $status -eq 0 && "$answer" == "published=true" ]] ||
    fail "a 503 then a 200 should be published=true, got status $status, '$answer': $(cat "$SANDBOX/log")"

# check-semver.sh refuses to pick a baseline when crates.io cannot say.
printf '503\n' > "$SANDBOX/codes"
: > "$SANDBOX/calls"
if (cd "$REPO" && PATH="$SANDBOX/bin:$PATH" CURL_CODES="$SANDBOX/codes" CURL_CALLS="$SANDBOX/calls" \
    CRATES_IO_RETRY_DELAY=0 bash scripts/check-semver.sh >"$SANDBOX/semver.log" 2>&1); then
    fail "check-semver.sh passed although crates.io answered 503: $(cat "$SANDBOX/semver.log")"
fi
grep -q 'against main' "$SANDBOX/semver.log" &&
    fail "check-semver.sh read a 503 as unpublished and compared against main"

echo "PASS  the crates.io check answers 200 and 404, and fails on anything else"
