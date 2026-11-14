#!/usr/bin/env bash
# The workflows that gate main cancel only what a newer run supersedes (#208):
#
#   - an ordinary pull request's runs share one group per workflow and pull
#     request number, and a newer head cancels the run before it;
#   - a red: pull request's run is the failing test's evidence (AGENTS.md):
#     a group naming its head commit, never cancelled, never shared with the
#     fix push that follows it;
#   - pushes to main, merge-queue runs and manual dispatches each keep their
#     verdict: never cancelled, and a group per run and attempt, because
#     GitHub keeps only one pending run per group even with cancelling off;
#   - ci-ok runs after every success and failure of every event, so a failed
#     or skipped job fails the required gate rather than skipping it; and
#     after the cancellation of a red:, push, merge-queue or dispatch run,
#     where test-ci-cancellation.sh holds its cancellation step to failing.
#     In a cancelled ordinary pull request run alone its condition is false
#     (#216): GitHub then cancels ci-ok with the rest of the run, without a
#     runner, so the superseded run completes and the newer head's run starts
#     at once. Superseded or cancelled by hand with nothing newer, the run's
#     ci-ok concludes `cancelled` (as the jobs whose implicit success() went
#     false did in run 43528816818), which never passes a required check. It
#     is never skipped: the condition is true until the run is cancelled.
#
# The expressions are evaluated here as GitHub evaluates them (startsWith
# ignores case; && and || return an operand; null reads as ''; format fills
# {0}, {1} ...), for each event the workflows run on, so a reworded
# expression is judged by what it does, not by how it reads.
#
#   bash tests/scripts/test-red-runs-uncancelled.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() { echo "FAIL  $*" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
python3 -c 'import yaml' 2>/dev/null ||
    fail "the python3 yaml module is required (pip install pyyaml)"

python3 - "$REPO/.github/workflows" <<'PY' || exit 1
import os, re, sys, yaml

root = sys.argv[1]
problems = []

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
    py = re.sub(r"\b(always|cancelled|failure|success)\(\)", r"_\1()", py)
    py = py.replace("&&", " and ").replace("||", " or ")
    py = re.sub(r"!(?!=)", " not ", py)
    py = re.sub(r"\bgithub((?:\.[A-Za-z_]+)+)", lambda m: "_g(%r)" % m.group(1)[1:], py)
    status = ctx["status"]
    return eval(py, {"__builtins__": {}, "_sw": starts_with, "_fmt": fill,
                     "_g": lambda p: lookup(ctx, p),
                     "_always": lambda: True,
                     "_cancelled": lambda: status == "cancelled",
                     "_failure": lambda: status == "failure",
                     "_success": lambda: status == "success"})

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

def condition(expr, ctx):
    # A job `if` may omit ${{ }}; one without a status function gets success().
    expr = str(expr).strip()
    m = re.fullmatch(r"\$\{\{(.*)\}\}", expr, re.S)
    expr = m.group(1) if m else expr
    if not re.search(r"\b(always|cancelled|failure|success)\(\)", expr):
        expr = "success() && (%s)" % expr
    return bool(evaluate(expr, ctx))

def context(workflow, event_name, title=None, number=7, head="a" * 40, run_id=1,
            run_attempt=1, status="success"):
    # github.sha is the head for a push, a test merge commit for a pull request.
    ctx = {"workflow": workflow, "event_name": event_name, "sha": head,
           "run_id": run_id, "run_attempt": run_attempt, "status": status,
           "ref": "refs/heads/main", "ref_name": "main", "head_ref": "", "event": {}}
    if title is not None:
        ctx.update(sha="%040x" % run_id, ref="refs/pull/%d/merge" % number, ref_name="%d/merge" % number,
                   head_ref="fix/run-end-bound")
        ctx["event"] = {"pull_request": {"number": number, "title": title, "head": {"sha": head}}}
    return ctx

groups = {}
for name in ("ci.yml", "image-safety.yml"):
    workflow = yaml.safe_load(open(os.path.join(root, name))) or {}
    conc = workflow.get("concurrency")
    if not isinstance(conc, dict) or "group" not in conc or "cancel-in-progress" not in conc:
        problems.append("%s has no concurrency group and cancel-in-progress" % name)
        continue
    wf = workflow.get("name") or name

    def run(*args, **kwargs):
        ctx = context(wf, *args, **kwargs)
        return render(conc["group"], ctx), render(conc["cancel-in-progress"], ctx) == "true"

    fix = "the run-end bound is checked"
    first, first_cancels = run("pull_request", fix)
    newer, _ = run("pull_request", fix, head="b" * 40, run_id=2)
    other, _ = run("pull_request", fix, number=8, head="c" * 40, run_id=3)
    if not first_cancels:
        problems.append("%s: a newer head of a pull request does not cancel the run it supersedes" % name)
    if first != newer:
        problems.append("%s: two heads of one pull request land in groups %r and %r, so neither supersedes the other" % (name, first, newer))
    if first == other:
        problems.append("%s: two pull requests share group %r and cancel each other" % (name, first))
    groups.setdefault(first, []).append(name)

    for title in ("red: the run-end bound is checked", "RED: the run-end bound is checked"):
        red, red_cancels = run("pull_request", title)
        red_again, _ = run("pull_request", title, head="b" * 40, run_id=2)
        red_retry, _ = run("pull_request", title, run_attempt=2)
        if red_cancels:
            problems.append("%s: a %r pull request's run cancels the run before it" % (name, title))
        if "a" * 40 not in red:
            problems.append("%s: a red: run's group %r does not name its head commit" % (name, red))
        if len({red, red_again, red_retry}) != 3:
            problems.append("%s: red: runs share a pending slot, so a newer one replaces the red run while it waits" % name)
        if red == first:
            problems.append("%s: the red run and the fix push share group %r, so the fix push cancels the red run" % (name, red))

    for event in ("push", "merge_group", "workflow_dispatch"):
        original, cancels = run(event)
        duplicate, _ = run(event, run_id=2)
        retry, _ = run(event, run_attempt=2)
        if cancels:
            problems.append("%s: a %s run is cancelled by a newer one" % (name, event))
        if len({original, duplicate, retry}) != 3:
            problems.append("%s: %s runs of the same commit or their attempts share a pending slot" % (name, event))
        if original == first:
            problems.append("%s: a %s run shares the pull request group %r" % (name, event, first))

    gate = (workflow.get("jobs") or {}).get("ci-ok")
    if name == "ci.yml":
        if not gate:
            problems.append("ci.yml has no ci-ok job")
            continue
        for event, title in (("pull_request", fix), ("pull_request", "red: x"), ("pull_request", "RED: x"),
                             ("push", None), ("merge_group", None), ("workflow_dispatch", None)):
            kind = ("%s pull request" % title.split(":")[0]) if title and ":" in title else event
            for status in ("success", "failure", "cancelled"):
                runs = condition(gate.get("if", ""), context(wf, event, title, status=status))
                superseded = event == "pull_request" and title == fix and status == "cancelled"
                if superseded and runs:
                    problems.append("ci-ok runs in a cancelled ordinary pull request run, so the superseded run "
                                    "waits for a runner and holds the newer head's run in the group (#216)")
                if not superseded and not runs:
                    problems.append("ci-ok does not run in a %s %s run: %s" % (
                        status, kind, "a skipped required check passes" if status != "cancelled"
                        else "a cancelled run that is evidence or gates main must fail ci-ok"))

for group, names in groups.items():
    if len(names) > 1:
        problems.append("%s share pull request group %r and cancel each other" % (" and ".join(names), group))
if problems:
    sys.exit("\n".join("FAIL  " + p for p in problems))
PY

echo "concurrency: superseded pull request runs cancel, ci-ok included; red, main and queue runs keep their verdicts"
