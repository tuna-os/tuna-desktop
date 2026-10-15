import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request

REPO = "tuna-os/tuna-desktop"
WORKFLOW = ".github/workflows/ci.yml"
JOB = "gtk-shell"


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as source:
        while block := source.read(1024 * 1024):
            result.update(block)
    return result.hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def source_tuple(root):
    if git(root, "diff", "HEAD", "--name-only").strip():
        raise ValueError("source checkout must remain clean")
    return {"sha": git(root, "rev-parse", "HEAD").decode().strip(),
            "tree": git(root, "rev-parse", "HEAD^{tree}").decode().strip(),
            "lock_sha256": digest(Path(root) / "Cargo.lock")}


def api(path):
    request = urllib.request.Request(f"https://api.github.com/repos/{REPO}/{path}", headers={
        "Authorization": f"Bearer {os.environ['GH_TOKEN']}", "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28"})
    with urllib.request.urlopen(request, timeout=20) as response:
        data = response.read(4 * 1024 * 1024 + 1)
    if len(data) > 4 * 1024 * 1024:
        raise ValueError("GitHub API response exceeds bounded admission size")
    return json.loads(data)


def validate_identity(run, jobs, commit, expected_sha, run_id, attempt, event):
    if (run.get("id") != run_id or run.get("run_attempt") != attempt
            or run.get("repository", {}).get("full_name") != REPO
            or run.get("head_sha") != expected_sha or run.get("path") != WORKFLOW
            or run.get("event") != event or event not in ("workflow_dispatch", "pull_request", "push")
            or run.get("status") not in ("in_progress", "queued") or run.get("conclusion") is not None
            or commit.get("sha") != expected_sha):
        # Retain the actual public identity fields and allowed run statuses
        # when refusing. A later API poll cannot reconstruct
        # the values seen by this invocation.
        observed = {key: run.get(key) for key in
                    ("id", "run_attempt", "head_sha", "path", "event", "status", "conclusion")}
        observed["repository"] = run.get("repository", {}).get("full_name")
        observed["commit_sha"] = commit.get("sha")
        expected = {"id": run_id, "run_attempt": attempt, "head_sha": expected_sha,
                    "path": WORKFLOW, "event": event, "status": ["in_progress", "queued"],
                    "conclusion": None, "repository": REPO, "commit_sha": expected_sha}
        raise ValueError("CI repository/run/attempt/source/event/workflow mismatch: " +
                         json.dumps({"expected": expected, "observed": observed}, sort_keys=True))
    # The aggregate run may report queued while this exact job is running.
    # Admission still requires this exact job running and its exact green
    # prerequisite, with matching source/run/attempt for both below.
    current = [j for j in jobs if j.get("name") == JOB]
    check = [j for j in jobs if j.get("name") == "check"]
    if (len(current) != 1 or current[0].get("status") != "in_progress"
            or current[0].get("conclusion") is not None
            or len(check) != 1 or check[0].get("status") != "completed"
            or check[0].get("conclusion") != "success"):
        raise ValueError("current comparison job and exact green prerequisite check required")
    for job in (current[0], check[0]):
        if job.get("head_sha") != expected_sha or job.get("run_id") != run_id or job.get("run_attempt") != attempt:
            raise ValueError("CI job source/run/attempt mismatch")
    return {"repository": REPO, "workflow": WORKFLOW, "run": run_id, "attempt": attempt,
            "event": event, "head": expected_sha, "tree": commit["tree"]["sha"],
            "job_id": current[0]["id"], "check_job_id": check[0]["id"]}


def current_identity(root):
    source = source_tuple(root)
    run_id, attempt = int(os.environ["GITHUB_RUN_ID"]), int(os.environ["GITHUB_RUN_ATTEMPT"])
    if os.environ.get("GITHUB_REPOSITORY") != REPO or os.environ.get("GITHUB_JOB") != JOB:
        raise ValueError("wrong CI repository/job")
    jobs = api(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100")
    if jobs.get("total_count", 101) > 100:
        raise ValueError("CI job admission requires complete bounded job page")
    result = validate_identity(api(f"actions/runs/{run_id}"), jobs.get("jobs", []),
                               api(f"git/commits/{source['sha']}"), source["sha"],
                               run_id, attempt, os.environ["GITHUB_EVENT_NAME"])
    if result["tree"] != source["tree"]:
        raise ValueError("GitHub source tree does not match checkout")
    return result
