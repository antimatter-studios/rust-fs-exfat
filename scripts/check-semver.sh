#!/usr/bin/env bash
# check-semver.sh -- `chore check:semver`: refuse a public-API break the
# version in Cargo.toml does not declare.
#
# ONCE THIS CRATE IS ON CRATES.IO the baseline is its newest release, and
# this is rust-fs-core's semver-check.sh, run in place.
#
# UNTIL THEN there is no release to compare with, and the family script
# refuses an unpublished crate (it has no former name to fall back on). The
# baseline is then main: a pull request that breaks the API main has must
# move the version in Cargo.toml with it, so the first release's version
# says what changed since the last time anyone could have depended on it.
# That is a real check, not a skip; it needs the history (CI checks out with
# fetch-depth 0).
#
# Whether the crate is published is crates.io's answer, read by
# scripts/crates-io-published.sh: 200 or 404, retried otherwise, and a
# failure when crates.io cannot say, never a guess at the baseline.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"
crate="$(sed -n 's/^name *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"

answer="$(bash scripts/crates-io-published.sh "$crate")"
case "$answer" in
    published=true) exec bash ../rust-fs-core/scripts/semver-check.sh ;;
    published=false) ;;
    *) echo "check-semver: crates-io-published.sh answered '$answer'" >&2; exit 1 ;;
esac

git fetch --quiet origin main 2>/dev/null || true
base="$(git merge-base HEAD origin/main 2>/dev/null)" ||
    { echo "check-semver: no merge base with origin/main (a shallow clone? fetch the history)" >&2; exit 1; }
cargo semver-checks --version >/dev/null 2>&1 ||
    { echo "check-semver: cargo-semver-checks is not installed: cargo install cargo-semver-checks --locked --version ${CARGO_SEMVER_CHECKS_VERSION:-0.50.0}" >&2; exit 1; }
# The baseline is main's tree, unpacked in a scratch directory: a
# --baseline-rev checkout would land inside target/ and share its lock.
scratch="$(mktemp -d "${TMPDIR:-/tmp}/exfat-semver.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
git archive "$base" | tar -x -C "$scratch"
echo "check-semver: $crate is unpublished; against main at ${base:0:12}"
cargo semver-checks check-release --package "$crate" --baseline-root "$scratch"
