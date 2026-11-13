#!/usr/bin/env bash
# Every required CI run retains its verdict, including a red-test run and
# duplicate events for the same commit. GitHub keeps only one pending run per
# concurrency group even with cancelling off, so each run and attempt needs its
# own group. Ordinary runs must now remain independent too (#167).
#
# The concurrency expressions are evaluated here as GitHub evaluates them
# (startsWith ignores case; && and || return an operand; null reads as '';
# format fills {0}, {1} ...),
# for each event the workflow runs on, so a reworded expression is judged by
# what it does, not by how it reads.
#
#   bash tests/scripts/test-red-runs-uncancelled.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() { echo "FAIL  $*" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
python3 -c 'import yaml' 2>/dev/null ||
    fail "the python3 yaml module is required (pip install pyyaml)"

python3 - "$REPO/.github/workflows/ci.yml" <<'PY' || exit 1
import re, sys, yaml

conc = (yaml.safe_load(open(sys.argv[1])) or {}).get("concurrency")
if not isinstance(conc, dict) or "group" not in conc or "cancel-in-progress" not in conc:
    sys.exit("FAIL  ci.yml has no concurrency group and cancel-in-progress")

def lookup(ctx, path):
    for key in path.split("."):
        ctx = ctx.get(key) if isinstance(ctx, dict) else None
    return ctx

def starts_with(s, prefix):
    return str("" if s is None else s).lower().startswith(str(prefix).lower())

def fill(template, *args):
    return re.sub(r"\{(\d+)\}", lambda m: text(args[int(m.group(1))]), template)

def evaluate(expr, ctx):
    py = re.sub(r"\bstartsWith\(", "_sw(", expr)
    py = re.sub(r"\bformat\(", "_fmt(", py)
    py = py.replace("&&", " and ").replace("||", " or ")
    py = re.sub(r"!(?!=)", " not ", py)
    py = re.sub(r"\bgithub((?:\.[A-Za-z_]+)+)", lambda m: "_g(%r)" % m.group(1)[1:], py)
    return eval(py, {"__builtins__": {}, "_sw": starts_with, "_fmt": fill, "_g": lambda p: lookup(ctx, p)})

def text(value):
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)

def render(template, ctx):
    template = str(template)
    if template in ("true", "false"):
        return template
    return re.sub(r"\$\{\{(.*?)\}\}", lambda m: text(evaluate(m.group(1), ctx)), template)

def run(event_name, title=None, head="a" * 40, run_id=1, run_attempt=1):
    ctx = {"workflow": "CI", "event_name": event_name, "sha": head[::-1],
           "run_id": run_id, "run_attempt": run_attempt,
           "ref": "refs/heads/main", "ref_name": "main", "head_ref": "", "event": {}}
    if title is not None:
        ctx.update(ref="refs/pull/7/merge", ref_name="7/merge", head_ref="fix/run-end-bound")
        ctx["event"] = {"pull_request": {"number": 7, "title": title, "head": {"sha": head}}}
    return render(conc["group"], ctx), render(conc["cancel-in-progress"], ctx) == "true"

red_group, red_cancels = run("pull_request", "red: the run-end bound is checked")
red_again, _ = run("pull_request", "red: the run-end bound is checked", head="b" * 40, run_id=2)
fix_group, fix_cancels = run("pull_request", "the run-end bound is checked", head="c" * 40, run_id=3)
fix_again, _ = run("pull_request", "the run-end bound is checked", head="d" * 40, run_id=4)
problems = []
if red_cancels:
    problems.append("a red: pull request's run cancels the run before it")
if red_group == red_again:
    problems.append("two pushes to a red: pull request share group %r, so the second replaces the red run while it waits" % red_group)
if fix_cancels or fix_group == fix_again:
    problems.append("an ordinary pull request run can cancel or replace the run it supersedes")
if red_group == fix_group:
    problems.append("the red run and the fix push share group %r, so the fix push cancels the red run" % fix_group)
for event in ("push", "merge_group", "workflow_dispatch"):
    if run(event)[1]:
        problems.append("a %s run is cancelled by a newer one" % event)
for event, title in (("pull_request", "red: evidence"), ("pull_request", "fix: evidence"),
                     ("push", None), ("merge_group", None), ("workflow_dispatch", None)):
    original, _ = run(event, title)
    duplicate, _ = run(event, title, run_id=2)
    retry, _ = run(event, title, run_attempt=2)
    if len({original, duplicate, retry}) != 3:
        problems.append("%s runs of the same commit or their attempts share a pending slot" % event)
if problems:
    sys.exit("\n".join("FAIL  " + p for p in problems))
PY

echo "red runs: every run and attempt retains its own verdict and pending slot"
