#!/usr/bin/env bash
# Was this cancelled CI run superseded? Exits 0 only when it was, so ci-ok's
# cancellation step reads `ci-superseded.sh || ci-ok.sh` (#208).
#
# ci.yml cancels an ordinary pull request's run when a newer run of the same
# workflow and pull request starts. ci-ok still runs in the cancelled run
# (`if: always()`, as rust-fs-core's ci-gate.sh requires), and a failed ci-ok
# there would sit beside the newer run's verdict on the same commit when both
# ran for one head (#167). GitHub does not say why a run was cancelled, so
# this asks the one thing that makes the cancellation harmless: does a NEWER
# run of this workflow exist for this pull request? Its ci-ok then reports
# the verdict. A run cancelled with no newer run behind it -- by hand, or for
# any other reason -- is not superseded, and its ci-ok fails.
#
# Never superseded: anything but a pull_request run, and a `red:` pull
# request's run (never cancelled by concurrency; its failure is evidence).
# Any lookup that fails answers "not superseded": the gate fails closed.
#
# Reads EVENT, TITLE, PR, REPO and RUN_ID; calls `gh api` (GH_TOKEN, with
# actions: read).
set -uo pipefail

not() { echo "not superseded: $*"; exit 1; }

[ "${EVENT:-}" = pull_request ] || not "a ${EVENT:-missing} run"
shopt -s nocasematch
case "${TITLE:-}" in red:*) not "a red: pull request's run is its evidence" ;; esac
shopt -u nocasematch
case "${PR:-}" in '' | *[!0-9]*) not "invalid pull request number '${PR:-}'" ;; esac
case "${RUN_ID:-}" in '' | *[!0-9]*) not "invalid run id '${RUN_ID:-}'" ;; esac
[ -n "${REPO:-}" ] || not "no repository"

this="$(gh api "repos/$REPO/actions/runs/$RUN_ID" --jq '"\(.workflow_id) \(.head_branch)"')" ||
    not "this run could not be read"
read -r workflow branch <<<"$this"
case "$workflow" in '' | *[!0-9]*) not "this run names no workflow" ;; esac
[ -n "$branch" ] || not "this run names no branch"

newer="$(gh api -X GET "repos/$REPO/actions/workflows/$workflow/runs" \
    -f event=pull_request -f branch="$branch" -F per_page=100 \
    --jq "[.workflow_runs[] | select(.id > $RUN_ID and any(.pull_requests[]; .number == $PR)) | .id] | min // empty")" ||
    not "the newer runs could not be listed"
[ -n "$newer" ] || not "no newer run of pull request #$PR"
echo "superseded by run $newer of pull request #$PR; its ci-ok reports the verdict"
