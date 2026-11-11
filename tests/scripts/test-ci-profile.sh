#!/usr/bin/env bash
# The workflows are held to what their headers say they do.
#
# rust-fs-ext4 keeps these guards in tests/ci_profile.rs; here they are a
# shell test, as rust-fs-core's ci-gate.sh argues a check that parses YAML
# and compares strings should be: it exercises nothing the crate ships, and
# as a cargo test it would count towards the unit tier's floor.
#
#   1. ci.yml runs on every pull request, whatever its base (every layer of
#      the stack is based on the one below it), in the merge queue and on
#      main.
#   2. ci-ok carries `if: always()`, needs every other job, and its verdict,
#      scripts/ci-ok.sh, judges exactly those jobs: a job missing from
#      either is a job that gates nothing.
#   3. EVERY JOB RUNS CHORE TASKS: each job runs the tasks its header line
#      names, every `chore <task>` in a workflow names a task chores.yml
#      has, and the unit job proves there are no fixture images before its
#      tier runs.
#   4. A tier-log upload waits for its tier: `if: always() &&
#      steps.tier.outcome != 'skipped'`, after a step with id `tier` that
#      runs a task writing a tier log, named tier-logs-<job>, refusing an
#      empty path (rust-fs-ext4 #306). A job that runs a tier keeps its log.
#   5. fuzz.yml keeps its reproducers on a failed or cancelled run, and its
#      fuzzing budget fits inside its timeout with the builds.
#   6. No `run:` interpolates a workflow input: an expression is
#      substituted before the shell parses it, so an input reaches a script
#      through `env:` only.
#
#   bash tests/scripts/test-ci-profile.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() { echo "FAIL  $*" >&2; exit 1; }

command -v python3 >/dev/null 2>&1 || fail "python3 is required"
python3 -c 'import yaml' 2>/dev/null ||
    fail "the python3 yaml module is required (pip install PyYAML)"

python3 - "$REPO" <<'PY' || exit 1
import os, re, sys, yaml

root = sys.argv[1]
problems = []

def load(path):
    with open(os.path.join(root, path)) as f:
        return yaml.safe_load(f) or {}

ci = load(".github/workflows/ci.yml")
fuzz = load(".github/workflows/fuzz.yml")
chores_doc = load("chores.yml")
chores = (chores_doc.get("tasks") or {})
# Tasks of an included taskfile (`vm:up` from the harness's vm.chores.yml)
# are named by their include's key; the file is a sibling's, not this tree's.
included = set((chores_doc.get("includes") or {}).keys())
workflows = {"ci.yml": ci, "fuzz.yml": fuzz}

def triggers(doc):
    t = doc.get("on", doc.get(True)) or {}
    if isinstance(t, str):
        return {t: None}
    if isinstance(t, list):
        return {k: None for k in t}
    return t

def steps(job):
    return job.get("steps") or []

def chore_tasks(run):
    return re.findall(r"(?:^|[\s;&|(])chore\s+([a-z][a-z0-9:-]*)", run or "")

def task_cmds(name, seen=()):
    """Every command a chore task runs, through its `task:` items."""
    task = chores.get(name) or {}
    out = []
    for c in task.get("cmds") or []:
        if isinstance(c, dict) and "task" in c:
            if c["task"] not in seen:
                out += task_cmds(c["task"], seen + (name,))
        elif isinstance(c, str):
            out.append(c)
            for sub in chore_tasks(c):
                if sub not in seen:
                    out += task_cmds(sub, seen + (name,))
    return out

def writes_tier_log(name):
    return any("rust-fs-core/scripts/tier.sh" in c for c in task_cmds(name))

# 1. Triggers.
on = triggers(ci)
for event in ("pull_request", "merge_group", "push"):
    if event not in on:
        problems.append(f"ci.yml does not run on {event}")
pr = on.get("pull_request") or {}
if isinstance(pr, dict) and (pr.get("branches") or pr.get("branches-ignore")):
    problems.append("ci.yml filters pull requests by base branch; a stacked layer would run nothing")
push = on.get("push") or {}
if "main" not in ((push or {}).get("branches") or []):
    problems.append("ci.yml does not run on a push to main")

# 2. ci-ok.
jobs = ci.get("jobs") or {}
ok = jobs.get("ci-ok")
if not ok:
    problems.append("ci.yml has no ci-ok job")
else:
    if str(ok.get("if", "")).replace("${{", "").replace("}}", "").strip() != "always()":
        problems.append("ci-ok does not carry `if: always()`")
    needs = ok.get("needs") or []
    needs = [needs] if isinstance(needs, str) else needs
    others = sorted(j for j in jobs if j != "ci-ok")
    if sorted(needs) != others:
        problems.append(f"ci-ok needs {sorted(needs)}, not every other job {others}")
    script = open(os.path.join(root, "scripts/ci-ok.sh")).read()
    m = re.search(r'^all="([^"]*)"$', script, re.M)
    listed = sorted((m.group(1) if m else "").split())
    if listed != sorted(needs):
        problems.append(f"scripts/ci-ok.sh judges {listed}, but ci-ok needs {sorted(needs)}")
    text = yaml.safe_dump(steps(ok))
    if not re.search(r"(?i)tojson\(\s*needs\s*\)", text) or "scripts/ci-ok.sh" not in text:
        problems.append("ci-ok does not hand toJSON(needs) to scripts/ci-ok.sh")

# 3. Every job runs chore tasks: the ones its header line names.
required = {
    "unit": ["build", "test:unit", "lint"],
    "images": ["test:images"],
    "test-arm64": ["lint", "build", "test:unit", "test:images"],
    "oracle-macos": ["build", "test:unit", "test:images", "test:oracle-macos"],
    "suite-in-vm": ["siblings", "test:vm", "vm:status"],
    "test": ["siblings", "tools", "test:oracle", "test:kernel", "test:third", "vm:status"],
    "package": ["test:package"],
    "semver": ["check:semver"],
    "msrv": ["check:msrv"],
    "wasm": ["tools:wasm", "test:wasm"],
    "cli": ["cli:install", "test:cli"],
    "provenance": ["check:provenance"],
    "scripts": ["test:scripts"],
    "fixtures": ["siblings", "tools", "fixtures", "vm:status"],
}
for name, tasks in required.items():
    job = jobs.get(name)
    if job is None:
        problems.append(f"ci.yml has no `{name}` job")
        continue
    ran = [t for s in steps(job) for t in chore_tasks(s.get("run"))]
    if ran != tasks:
        problems.append(f"job `{name}` runs chore {ran}, not {tasks}")
    if "timeout-minutes" not in job:
        problems.append(f"job `{name}` has no timeout-minutes")
if not str(jobs.get("oracle-macos", {}).get("runs-on", "")).startswith("macos"):
    problems.append("oracle-macos does not run on macOS, where newfs_exfat and fsck_exfat are")
if jobs.get("test-arm64", {}).get("runs-on") != "ubuntu-24.04-arm":
    problems.append("test-arm64 does not run on GitHub's aarch64 runner")

unit_runs = [s.get("run") or "" for s in steps(jobs.get("unit", {}))]
guard = next((i for i, r in enumerate(unit_runs) if "test-disks/*.img" in r and "exit 1" in r), None)
tier = next((i for i, r in enumerate(unit_runs) if "test:unit" in chore_tasks(r)), None)
if guard is None or tier is None or guard > tier:
    problems.append("the unit job does not refuse fixture images before its tier runs")

for wf, doc in workflows.items():
    for jname, job in (doc.get("jobs") or {}).items():
        for s in steps(job):
            for t in chore_tasks(s.get("run")):
                if t not in chores and t.split(":")[0] not in included:
                    problems.append(f"{wf} {jname} runs `chore {t}`, which chores.yml does not have")

# 4. Tier-log uploads wait for their tier.
for wf, doc in workflows.items():
    for jname, job in (doc.get("jobs") or {}).items():
        st = steps(job)
        ids = [s.get("id") for s in st]
        uploads = [i for i, s in enumerate(st)
                   if "upload-artifact" in str(s.get("uses", ""))
                   and "tmp/logs" in str((s.get("with") or {}).get("path", ""))]
        tier_at = ids.index("tier") if "tier" in ids else None
        if tier_at is not None:
            tasks = chore_tasks(st[tier_at].get("run"))
            if not tasks or not writes_tier_log(tasks[0]):
                problems.append(f"{wf} {jname}: the step with id `tier` runs no task that writes a tier log")
            if not uploads:
                problems.append(f"{wf} {jname} runs a tier and keeps no tier log")
        for i in uploads:
            s = st[i]
            cond = re.sub(r"\s+", "", str(s.get("if", "")).replace("${{", "").replace("}}", ""))
            if cond != "always()&&steps.tier.outcome!='skipped'":
                problems.append(f"{wf} {jname}: a tier-log upload runs on `{s.get('if')}`, "
                                "not `always() && steps.tier.outcome != 'skipped'`")
            if tier_at is None or tier_at > i:
                problems.append(f"{wf} {jname}: a tier-log upload comes before any step with id `tier`")
            w = s.get("with") or {}
            if w.get("if-no-files-found") != "error":
                problems.append(f"{wf} {jname}: a tier-log upload does not refuse an empty path")
            want = "tier-logs-fuzz" if wf == "fuzz.yml" else f"tier-logs-{jname}"
            if w.get("name") != want:
                problems.append(f"{wf} {jname}: the tier logs are uploaded as {w.get('name')!r}, not {want!r}")

# 5. fuzz.yml.
fjob = (fuzz.get("jobs") or {}).get("fuzz") or {}
repro = [s for s in steps(fjob) if "upload-artifact" in str(s.get("uses", ""))
         and (s.get("with") or {}).get("name") == "fuzz-artifacts"]
if not repro:
    problems.append("fuzz.yml uploads no fuzz-artifacts")
else:
    cond = re.sub(r"\s+", "", str(repro[0].get("if", "")))
    if "failure()" not in cond or "cancelled()" not in cond:
        problems.append("fuzz.yml keeps its reproducers on "
                        f"`{repro[0].get('if')}`, not `failure() || cancelled()`")
fuzz_all = open(os.path.join(root, "scripts/fuzz-all.sh")).read()
m = re.search(r"^total_budget=(\d+)$", fuzz_all, re.M)
timeout = fjob.get("timeout-minutes")
if not m or not isinstance(timeout, int):
    problems.append("scripts/fuzz-all.sh sets no total_budget, or fuzz.yml's job no timeout-minutes")
elif int(m.group(1)) + 30 * 60 > timeout * 60:
    problems.append(f"fuzz-all.sh may fuzz {m.group(1)}s, which with 1800s for the builds "
                    f"does not fit fuzz.yml's {timeout} minutes")
if set(triggers(fuzz)) - {"schedule", "workflow_dispatch"}:
    problems.append("fuzz.yml runs on more than its schedule and dispatch: a fresh finding "
                    "would fail an unrelated pull request")

# 6. No run: interpolates an input.
for wf, doc in workflows.items():
    for jname, job in (doc.get("jobs") or {}).items():
        for s in steps(job):
            if re.search(r"\$\{\{[^}]*\b(inputs|github\.event\.inputs)\.", s.get("run") or ""):
                problems.append(f"{wf} {jname}: step {s.get('name')!r} interpolates an input into run:")

if problems:
    sys.exit("\n".join("FAIL  " + p for p in problems))
PY

echo "ci profile: every job runs its chore tasks, ci-ok judges every job, and tier logs wait for their tier"
