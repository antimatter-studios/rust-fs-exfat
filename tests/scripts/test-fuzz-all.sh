#!/usr/bin/env bash
# scripts/fuzz-all.sh must tell a clean run from a crash, from a run that failed,
# from a target with no corpus, and from a run that did nothing. A stub stands in
# for cargo in a sandbox copy of the tree, so this needs no nightly and no fuzzing.
set -uo pipefail

fails=0
ok() { printf 'ok    %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sandbox="$(mktemp -d "${TMPDIR:-/tmp}/test-fuzz-all.XXXXXX")" || exit 1
trap 'rm -r "$sandbox"' EXIT

mkdir -p "$sandbox/tree/scripts" "$sandbox/bin" "$sandbox/tree/fuzz/corpus/one" "$sandbox/tree/fuzz/corpus/two"
cp "$here/scripts/fuzz-all.sh" "$sandbox/tree/scripts/"
printf '[package]\nname = "stub"\n\n[[bin]]\nname = "one"\n\n[[bin]]\nname = "two"\n' >"$sandbox/tree/fuzz/Cargo.toml"
printf 'seed' >"$sandbox/tree/fuzz/corpus/one/seed"
printf 'seed' >"$sandbox/tree/fuzz/corpus/two/seed"

# Behaves as STUB says for target one, and as a clean run for every other target.
cat >"$sandbox/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STUB_CALLS"
target="$4"
behaviour="clean"
[ "$target" = one ] && behaviour="$STUB"
case "$behaviour" in
    clean) printf '#5000\tDONE   cov: 12 ft: 34 corp: 5/10b\nstat::number_of_executed_units: 5000\n' ;;
    idle) printf '#1\tDONE   cov: 0 ft: 0 corp: 1/1b\nstat::number_of_executed_units: 1\n' ;;
    crash) mkdir -p "fuzz/artifacts/$target"; : >"fuzz/artifacts/$target/crash-1"; exit 77 ;;
    broken) echo 'error: could not compile'; exit 101 ;;
esac
EOF
chmod +x "$sandbox/bin/cargo"

run() {
    output="$(cd "$sandbox/tree" && PATH="$sandbox/bin:$PATH" STUB="$1" STUB_CALLS="$sandbox/calls" \
        bash scripts/fuzz-all.sh "${@:2}" 2>&1)"
    status=$?
}

refuses() {
    : >"$sandbox/calls"
    run clean "${@:2}"
    if [ "$status" -eq 2 ] && [ ! -s "$sandbox/calls" ]; then
        ok "$1 is refused before anything runs"
    else
        fail "$1: status $status, calls: $(wc -l <"$sandbox/calls")"
    fi
}

refuses 'a budget that is a command' '1; false'
refuses 'a zero budget' 0
refuses 'a negative budget' -5
refuses 'a budget over the run total' 3000
refuses 'an extra argument' 5 6

: >"$sandbox/calls"
run clean 5
if [ "$status" -eq 0 ] && grep -q 'one: clean, 5000 inputs in 5s, coverage 12' <<<"$output" &&
    grep -q 'every target ran 5s' <<<"$output"; then
    ok 'a clean run reports its inputs and coverage'
else
    fail "clean run: status $status: $output"
fi
if grep -q -- '-max_total_time=5 ' "$sandbox/calls" && grep -q 'fuzz/corpus/one' "$sandbox/calls"; then
    ok 'the budget and the committed seeds reach the fuzzer'
else
    fail "fuzzer arguments: $(cat "$sandbox/calls")"
fi

run idle 5
if [ "$status" -eq 1 ] && grep -q 'too little fuzzing was done for: one' <<<"$output"; then
    ok 'a run that executed almost nothing fails'
else
    fail "idle run: status $status: $output"
fi

run crash 5
if [ "$status" -eq 1 ] && grep -q 'found crashes in: one' <<<"$output" &&
    grep -q 'two: clean' <<<"$output"; then
    ok 'a crash fails the run, names its target and still runs the rest'
else
    fail "crash run: status $status: $output"
fi

rm -r "$sandbox/tree/fuzz/artifacts"
run broken 5
if [ "$status" -eq 1 ] && grep -q 'failed to run for: one' <<<"$output"; then
    ok 'a run with no reproducer is reported as not having run'
else
    fail "broken run: status $status: $output"
fi

rm -r "$sandbox/tree/fuzz/corpus/one"
: >"$sandbox/calls"
run clean 5
if [ "$status" -eq 1 ] && grep -q 'no seed corpus for: one' <<<"$output" &&
    grep -q 'two: clean' <<<"$output" && ! grep -q 'fuzz run one' "$sandbox/calls"; then
    ok 'a target with no corpus fails, is not run, and the rest still run'
else
    fail "missing corpus: status $status: $output"
fi

[ "$fails" -eq 0 ] && echo 'test-fuzz-all: all checks passed'
exit $(( fails > 0 ))
