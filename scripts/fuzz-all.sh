#!/usr/bin/env bash
# Run every fuzz target for a bounded time, one process at a time.
#
# This is the explorer, not the gate. It may find something and fail; what it
# may not do is run unbounded, stop at the first finding, or report a run that
# did nothing as a clean one. Each target's whole output goes to
# tmp/logs/fuzz-<target>.log and each target prints one verdict line.
#
# Anything it finds belongs in fuzz/corpus/<target>/, where
# tests/fuzz_decoders.rs replays it on every pull request from then on.
#
# Usage: scripts/fuzz-all.sh [seconds-per-target]
set -uo pipefail

# The most fuzzing one run may do, in seconds, across every target. fuzz.yml's
# job has a 90-minute timeout and building takes most of the other half hour, so
# this is checked here, before anything runs, rather than found by the timeout.
total_budget=3600

# One input taking this long is a hang worth a reproducer, even on a busy machine.
input_timeout=60

[ $# -le 1 ] || { echo "usage: fuzz-all.sh [seconds-per-target]" >&2; exit 2; }
budget="${1-30}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here" || exit 1

targets=$(sed -n 's/^name = "\(.*\)"$/\1/p' fuzz/Cargo.toml | tail -n +2)
[ -n "$targets" ] || { echo "no fuzz targets declared in fuzz/Cargo.toml" >&2; exit 1; }
count=$(wc -w <<<"$targets" | tr -d ' ')

# The budget is data: it can arrive from a workflow_dispatch input. Zero is refused
# because libFuzzer reads -max_total_time=0 as no limit. The length check comes
# first so the arithmetic cannot overflow, and 10# stops octal.
if ! [[ "$budget" =~ ^[0-9]+$ ]] || [ "${#budget}" -gt 6 ] || [ $((10#$budget)) -eq 0 ]; then
    echo "::error::seconds per target must be a positive whole number, got '$budget'" >&2
    exit 2
fi
budget=$((10#$budget))
if [ $((budget * count)) -gt "$total_budget" ]; then
    echo "::error::${budget}s x $count targets is over the ${total_budget}s this run may fuzz" >&2
    exit 2
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/exfat-fuzz.XXXXXX")" || exit 1
trap 'rm -r "$scratch"' EXIT
mkdir -p tmp/logs

artifacts_of() {
    find "fuzz/artifacts/$1" -type f 2>/dev/null | wc -l | tr -d ' '
}

# Four verdicts, which must not be confused. A CRASH leaves a reproducer under
# fuzz/artifacts/<target>/ and exits non-zero. A MISSING CORPUS would let the
# fuzzer start from nothing and pass, which is a skip that looks like a run. A run
# that FAILED, such as a build error, exits non-zero and leaves no reproducer. And
# a run that exited zero having executed almost nothing proves nothing, so it fails
# too: only the count of executed inputs can tell it from a real run.
missing=""
broken=""
failed=""
idle=""
for target in $targets; do
    seeds="fuzz/corpus/$target"
    if [ ! -d "$seeds" ] || [ -z "$(find "$seeds" -type f -print -quit)" ]; then
        echo "::error::$target: no seed corpus: $seeds is missing or empty"
        missing="$missing $target"
        continue
    fi
    log="tmp/logs/fuzz-$target.log"
    # libFuzzer writes new inputs into the first corpus directory it is given, so
    # that one is throwaway and the committed seeds stay as committed.
    mkdir -p "$scratch/$target"
    before=$(artifacts_of "$target")
    if cargo +nightly fuzz run "$target" "$scratch/$target" "$seeds" -- \
        -max_total_time="$budget" -timeout="$input_timeout" -rss_limit_mb=2048 \
        -print_final_stats=1 >"$log" 2>&1; then
        runs=$(sed -n 's/^stat::number_of_executed_units: *//p' "$log" | tail -n 1)
        coverage=$(sed -n 's/.*DONE *cov: *\([0-9]*\).*/\1/p' "$log" | tail -n 1)
        if [ "${runs:-0}" -ge "$budget" ] && [ "${coverage:-0}" -gt 0 ]; then
            echo "$target: clean, ${runs} inputs in ${budget}s, coverage ${coverage}"
        else
            echo "::error::$target: ran only ${runs:-0} inputs with coverage ${coverage:-0}; see $log"
            idle="$idle $target"
        fi
    elif [ "$(artifacts_of "$target")" -gt "$before" ]; then
        echo "::error::$target: found an input that crashed or hung; see $log"
        failed="$failed $target"
    else
        echo "::error::$target: did not run, and left no reproducer; see $log"
        broken="$broken $target"
    fi
done

status=0
[ -z "$missing" ] || { echo "::error::no seed corpus for:$missing"; status=1; }
[ -z "$broken" ] || { echo "::error::the fuzzer failed to run for:$broken"; status=1; }
[ -z "$idle" ] || { echo "::error::too little fuzzing was done for:$idle"; status=1; }
if [ -n "$failed" ]; then
    echo "::error::fuzzing found crashes in:$failed"
    echo "Each reproducer is under fuzz/artifacts/<target>/. Commit it to"
    echo "fuzz/corpus/<target>/ so tests/fuzz_decoders.rs replays it from now on."
    status=1
fi
[ "$status" -eq 0 ] || exit "$status"
echo "every target ran ${budget}s without finding a crash"
