#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$REPO" <<'PY'
import os
import re
import subprocess
import sys
import shutil
import tempfile

import yaml

root = sys.argv[1]
with open(os.path.join(root, ".github/workflows/ci.yml")) as source:
    workflow = yaml.safe_load(source)

problems = []
# The concurrency groups and ci-ok's own condition are evaluated, event by
# event, in tests/scripts/test-red-runs-uncancelled.sh (#208). Here: when
# ci-ok runs in a cancelled run, it fails.
gate = workflow["jobs"]["ci-ok"]
cancellation = [step for step in gate["steps"] if step.get("if") == "cancelled()"]
if (gate.get("permissions") or {}).get("actions") != "read":
    problems.append("ci-ok needs actions: read to list the runs that supersede it")
verdict = [step for step in gate["steps"] if "toJSON(needs)" in str(step.get("env", {}).get("NEEDS", ""))]
if len(verdict) != 1 or str(verdict[0].get("if", "")).replace(" ", "") not in ("${{!cancelled()}}", "!cancelled()"):
    problems.append("ci-ok's job verdict must not run after a cancellation, which the step before it decides")
if len(cancellation) != 1:
    problems.append("ci-ok must handle workflow cancellation in exactly one step condition")
else:
    cancellation = cancellation[0]
    env = cancellation.get("env", {})
    if env.get("CANCELLED") != "true":
        problems.append("the cancellation step must pass a literal true cancellation state")
    wanted = {"REPO": "github.repository", "RUN_ID": "github.run_id", "EVENT": "github.event_name",
              "TITLE": "github.event.pull_request.title", "PR": "github.event.pull_request.number"}
    for name, context in wanted.items():
        if str(env.get(name, "")).replace(" ", "") != "${{" + context + "}}":
            problems.append(f"the cancellation step's {name} is not ${{{{ {context} }}}}")
    if cancellation.get("run") != "bash scripts/ci-superseded.sh || bash scripts/ci-ok.sh < /dev/null":
        problems.append("the cancellation step must pass only a superseded run and fail every other through the required gate")
    else:
        # `gh` is a stub answering from canned runs, through the real jq.
        stub = tempfile.mkdtemp()
        with open(os.path.join(stub, "gh"), "w") as gh:
            gh.write(r"""#!/usr/bin/env bash
[ -n "${STUB_FAIL:-}" ] && exit 1
filter=; url=
while [ $# -gt 0 ]; do
  case "$1" in --jq) filter="$2"; shift ;; repos/*) url="$1" ;; esac; shift
done
case "$url" in
  */actions/runs/*) echo '{"workflow_id": 9, "head_branch": "fix/x"}' ;;
  */workflows/9/runs) echo "{\"workflow_runs\": $STUB_RUNS}" ;;
  *) exit 1 ;;
esac | jq -r "$filter"
""")
        os.chmod(os.path.join(stub, "gh"), 0o755)
        newer = '[{"id": 50, "pull_requests": [{"number": 7}]}, {"id": 200, "pull_requests": [{"number": 7}]}]'
        older = '[{"id": 50, "pull_requests": [{"number": 7}]}, {"id": 100, "pull_requests": [{"number": 7}]}]'
        other = '[{"id": 200, "pull_requests": [{"number": 8}]}]'
        cases = [
            ("a newer run of the pull request", dict(STUB_RUNS=newer), 0),
            ("no newer run (cancelled by hand)", dict(STUB_RUNS=older), 1),
            ("a newer run of another pull request", dict(STUB_RUNS=other), 1),
            ("a red: pull request", dict(STUB_RUNS=newer, TITLE="Red: x"), 1),
            ("a merge_group run", dict(STUB_RUNS=newer, EVENT="merge_group", PR=""), 1),
            ("a push run", dict(STUB_RUNS=newer, EVENT="push", PR=""), 1),
            ("a failed lookup", dict(STUB_RUNS=newer, STUB_FAIL="1"), 1),
        ]
        for label, overrides, expected in cases:
            run_env = dict(os.environ, CANCELLED="true", REPO="o/r", RUN_ID="100", EVENT="pull_request",
                           TITLE="fix: x", PR="7", PATH=stub + os.pathsep + os.environ["PATH"])
            run_env.update(overrides)
            result = subprocess.run(["bash", "-c", cancellation["run"]], cwd=root, text=True,
                                    capture_output=True, env=run_env)
            if result.returncode != expected or (expected and "workflow cancelled" not in result.stdout):
                problems.append(f"cancelled with {label}: status {result.returncode}, wanted {expected}: {result.stdout.strip()}")
        shutil.rmtree(stub)
for job in workflow["jobs"].values():
    for step in job.get("steps", []):
        for value in step.get("env", {}).values():
            if re.search(r"\b(?:always|cancelled|failure|success)\s*\(", str(value)):
                problems.append("status functions are not allowed in step env expressions")

results = "".join(job + " success\n" for job in gate["needs"])
for cancelled, expected in (("false", 0), ("true", 1), ("invalid", 1)):
    result = subprocess.run(
        ["bash", os.path.join(root, "scripts/ci-ok.sh")],
        input=results,
        text=True,
        capture_output=True,
        env=dict(os.environ, PROFILE="quick", CODE="true", CANCELLED=cancelled),
    )
    if result.returncode != expected:
        problems.append(f"all jobs succeeded, cancelled={cancelled}: status {result.returncode}, wanted {expected}")

if problems:
    sys.exit("\n".join("FAIL  " + problem for problem in problems))
PY
echo "ci cancellation: only a superseded run passes the required gate"
