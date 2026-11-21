#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$REPO" <<'PY'
import os
import re
import subprocess
import sys

import yaml

root = sys.argv[1]
with open(os.path.join(root, ".github/workflows/ci.yml")) as source:
    workflow = yaml.safe_load(source)

problems = []
# The concurrency groups and ci-ok's own condition are evaluated, event by
# event, in tests/scripts/test-red-runs-uncancelled.sh (#208, #216): ci-ok
# reaches a cancelled run only when it is a red:, push, merge-queue or
# dispatch run, and GitHub cancels it with the rest of a cancelled ordinary
# pull request run. Here: when ci-ok does run in a cancelled run, it fails,
# whatever the event, and nothing in it asks whether the run was superseded.
gate = workflow["jobs"]["ci-ok"]
cancellation = [step for step in gate["steps"] if step.get("if") == "cancelled()"]
verdict = [step for step in gate["steps"] if "toJSON(needs)" in str(step.get("env", {}).get("NEEDS", ""))]
if len(verdict) != 1 or str(verdict[0].get("if", "")).replace(" ", "") not in ("${{!cancelled()}}", "!cancelled()"):
    problems.append("ci-ok's job verdict must not run after a cancellation, which the step before it decides")
if len(cancellation) != 1:
    problems.append("ci-ok must handle workflow cancellation in exactly one step condition")
else:
    cancellation = cancellation[0]
    if cancellation.get("env") != {"CANCELLED": "true"}:
        problems.append("the cancellation step must pass a literal true cancellation state and nothing else")
    if cancellation.get("run") != "bash scripts/ci-ok.sh < /dev/null":
        problems.append("the cancellation step must fail every run that reaches it through the required gate")
    for event, title in (("pull_request", "red: x"), ("pull_request", "RED: x"), ("push", ""),
                         ("merge_group", ""), ("workflow_dispatch", ""), ("pull_request", "fix: x")):
        run_env = dict(os.environ, EVENT=event, TITLE=title, PR="7" if title else "")
        run_env.update(cancellation["env"])
        result = subprocess.run(["bash", "-c", cancellation["run"]], cwd=root, text=True,
                                capture_output=True, env=run_env)
        if result.returncode != 1 or "workflow cancelled" not in result.stdout:
            problems.append(f"a cancelled {title or event} run passed ci-ok's cancellation step: "
                            f"status {result.returncode}: {result.stdout.strip()}")
if (gate.get("permissions") or {}) != {"contents": "read"}:
    problems.append("ci-ok reads no API: its permissions are contents: read alone")
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
echo "ci cancellation: every cancelled run that reaches ci-ok fails it"
