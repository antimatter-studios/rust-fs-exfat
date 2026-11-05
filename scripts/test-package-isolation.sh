#!/usr/bin/env bash
# chore package must leave nothing behind that a later test build links. Package
# verification builds the packaged copy under the same crate hash as the working
# tree's build, so in a shared target directory the next test, after an edit to
# the library, can link the packaged library and pass or fail against old code.
# This edits a copy of the tree after chore package and asks a test which library
# it linked. The test takes the cli feature, as the unit tier does.
set -uo pipefail

fails=0
ok() { printf 'ok    %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sandbox="$(mktemp -d "${TMPDIR:-/tmp}/test-package-isolation.XXXXXX")" || exit 1
trap 'rm -r "$sandbox"' EXIT
export CARGO_TARGET_DIR="$sandbox/target"
tree="$sandbox/tree"
mkdir "$tree"
(cd "$here" && git ls-files -z | tar --null -T - -cf -) | tar -xf - -C "$tree" || exit 1

if (cd "$tree" && chore package) >"$sandbox/package.log" 2>&1; then
    ok 'chore package passes on the copy'
else
    fail "chore package failed: $(tail -n 5 "$sandbox/package.log")"
fi

marker='edited after package verification'
sed -i.original "s/read outside device or arithmetic overflow/$marker/" "$tree/src/error.rs"
if grep -q "$marker" "$tree/src/error.rs"; then
    ok 'the library is edited after package verification'
else
    fail 'the edit to src/error.rs did not apply'
fi
cat >"$tree/tests/linked_library.rs" <<TEST
#[test]
fn the_test_links_the_edited_library() {
    assert_eq!(fs_exfat::Error::Bounds.to_string(), "$marker");
}
TEST
if (cd "$tree" && cargo test --locked --features cli --test linked_library) >"$sandbox/test.log" 2>&1; then
    ok 'a test built after package verification links the edited library'
else
    fail 'a test built after package verification did not link the edited library:'
    grep -E 'left:|right:|^error' "$sandbox/test.log"
fi

[ "$fails" -eq 0 ] && echo 'test-package-isolation: all checks passed'
exit $(( fails > 0 ))
