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
concurrency = workflow.get("concurrency", {})
if concurrency.get("cancel-in-progress") is not False:
    problems.append("required CI must not auto-cancel an in-progress run")
for identity in ("run_id", "run_attempt"):
    if not re.search(r"\$\{\{\s*github\." + identity + r"\s*\}\}", concurrency.get("group", "")):
        problems.append("required CI needs a separate pending slot for every " + identity)

gate = workflow["jobs"]["ci-ok"]
if gate.get("if") != "always()":
    problems.append("ci-ok must always run, even after cancellation")
cancellation = [step for step in gate["steps"] if step.get("if") == "cancelled()"]
if len(cancellation) != 1:
    problems.append("ci-ok must handle workflow cancellation in exactly one step condition")
else:
    cancellation = cancellation[0]
    if cancellation.get("env", {}).get("CANCELLED") != "true":
        problems.append("the cancellation step must pass a literal true cancellation state")
    if cancellation.get("run") != "bash scripts/ci-ok.sh < /dev/null":
        problems.append("the cancellation step must fail through the required gate without job input")
    else:
        result = subprocess.run(
            ["bash", "-c", cancellation["run"]],
            cwd=root,
            text=True,
            capture_output=True,
            env=dict(os.environ, **cancellation.get("env", {})),
        )
        if result.returncode != 1 or "workflow cancelled" not in result.stdout:
            problems.append("the workflow cancellation step must report failure")
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
echo "ci cancellation: independent runs, and cancellation never passes the required gate"
