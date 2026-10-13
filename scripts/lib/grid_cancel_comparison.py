"""Admission and verdicts for CI's real old/fixed headed drag comparison.

The old source is reviewed observation-only code, not a product behavior flag.
The current API run may be in progress; the prerequisite check must be green.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request

REPO = "tuna-os/tuna-desktop"
WORKFLOW = ".github/workflows/ci.yml"
JOB = "grid-cancel-comparison"
OLD_SHA = "da1b7c40c7cc69f5800970e61cffa3d4e95c3f65"
OLD_PARENT = "416ce950cb4dabc7701f8c66ab1cdb680d493287"
OLD_PATCH = "0050999ead67c1fcc56a81402c6d2377ed39a2e234900eb35a6525654d4761ef"
ROLES = {
    "tuna-compositor": ("tuna-compositor", "bin", "crates/compositor/src/main.rs"),
    "tuna-shell-gtk": ("tuna-shell-gtk", "bin", "crates/shell-gtk/src/main.rs"),
    "tuna-shell-host": ("tuna-shell-host", "bin", "crates/shell-host/src/main.rs"),
    "tuna-ibus-bridge": ("tuna-shell-host", "bin", "crates/shell-host/src/bin/tuna-ibus-bridge.rs"),
    "icon-probe": ("tuna-compositor", "example", "crates/compositor/examples/icon-probe.rs"),
    "shortcut-inhibit-client": ("tuna-compositor", "example", "crates/compositor/examples/shortcut-inhibit-client.rs"),
}


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as source:
        while block := source.read(1024 * 1024):
            result.update(block)
    return result.hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def source_tuple(root):
    if git(root, "status", "--porcelain", "--untracked-files=all").strip():
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
            or run.get("status") != "in_progress" or run.get("conclusion") is not None
            or commit.get("sha") != expected_sha):
        # Preserve the admission predicates, but retain the actual public
        # identity fields when refusing. A later API poll cannot reconstruct
        # the values seen by this invocation.
        observed = {key: run.get(key) for key in
                    ("id", "run_attempt", "head_sha", "path", "event", "status", "conclusion")}
        observed["repository"] = run.get("repository", {}).get("full_name")
        observed["commit_sha"] = commit.get("sha")
        expected = {"id": run_id, "run_attempt": attempt, "head_sha": expected_sha,
                    "path": WORKFLOW, "event": event, "status": "in_progress",
                    "conclusion": None, "repository": REPO, "commit_sha": expected_sha}
        raise ValueError("CI repository/run/attempt/source/event/workflow mismatch: " +
                         json.dumps({"expected": expected, "observed": observed}, sort_keys=True))
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


def validate_old_source(root, commit):
    source = source_tuple(root)
    if (source["sha"] != OLD_SHA or commit.get("sha") != OLD_SHA
            or commit.get("tree", {}).get("sha") != source["tree"]
            or [p.get("sha") for p in commit.get("parents", [])] != [OLD_PARENT]
            or git(root, "rev-parse", "HEAD^").decode().strip() != OLD_PARENT):
        raise ValueError("old observation source identity mismatch")
    patch = git(root, "diff", OLD_PARENT, OLD_SHA, "--binary")
    if hashlib.sha256(patch).hexdigest() != OLD_PATCH:
        raise ValueError("old source observation-only patch differs from review")
    return source


def select_artifacts(messages, root, target):
    selected = {}
    root, target = Path(root).resolve(), Path(target).resolve()
    for message in messages:
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        name = message.get("target", {}).get("name")
        if name not in ROLES:
            continue
        package, kind, src = ROLES[name]
        executable = Path(message["executable"]).resolve()
        package_prefix = "path+" + (root / Path(*Path(src).parts[:2])).as_uri() + "#"
        if (message.get("target", {}).get("kind") != [kind]
                or message.get("profile", {}).get("test") is not False
                or not message.get("package_id", "").startswith(package_prefix)
                or re.search(rf"#{re.escape(package)}@[0-9]+\.[0-9]+\.[0-9]+$", message.get("package_id", "")) is None
                or Path(message.get("target", {}).get("src_path", "")).resolve() != root / src
                or not executable.is_relative_to(target)):
            raise ValueError(f"wrong Cargo build role for {name}")
        # Cargo can report an unchanged bin again in the examples build. Only
        # identical artifact records may coalesce; alternate executables fail.
        record = {"name": name, "package": package, "kind": kind, "source_path": str(root / src),
                  "cargo_executable": str(executable), "sha256": digest(executable)}
        if name in selected and selected[name] != record:
            raise ValueError(f"ambiguous Cargo build role {name}")
        selected[name] = record
    if set(selected) != set(ROLES):
        raise ValueError("missing required named build roles")
    return selected


def validate_live_admission(admission, harness_root, compositor_pid, shell_pid, expect_old):
    identity = current_identity(harness_root)
    if admission.get("ci") != identity or admission.get("harness") != source_tuple(harness_root):
        raise ValueError("comparison admission is stale")
    role = "old" if expect_old else "fixed"
    if admission.get("role") != role:
        raise ValueError("wrong old/fixed admission role")
    source_root = Path(admission["source_root"])
    source = source_tuple(source_root)
    if source != admission.get("source"):
        raise ValueError("binary source changed")
    if expect_old:
        validate_old_source(source_root, api(f"git/commits/{OLD_SHA}"))
    elif source != source_tuple(harness_root):
        raise ValueError("fixed binary must match the current harness source")
    files = admission.get("files", {})
    if set(files) != set(ROLES):
        raise ValueError("incomplete admitted build tuple")
    for name, record in files.items():
        package, kind, src = ROLES[name]
        if (record.get("name") != name or record.get("package") != package or record.get("kind") != kind
                or record.get("source_path") != str(source_root.resolve() / src)
                or digest(record["cargo_executable"]) != record.get("sha256")
                or digest(record["admitted_executable"]) != record.get("sha256")):
            raise ValueError("admitted executable/build role changed")
    for name, pid in (("tuna-compositor", compositor_pid), ("tuna-shell-gtk", shell_pid)):
        if digest(f"/proc/{pid}/exe") != files[name]["sha256"]:
            raise ValueError("live actor does not match admitted executable")


def old_negative(before, after):
    if not isinstance(before, dict) or not isinstance(after, dict):
        raise ValueError("missing old comparison state")
    for state in (before, after):
        if state.get("client_dnd_active") is not True or state.get("locked") is not False:
            raise ValueError("old source did not retain the actual unlocked drag")
    if before.get("overview_open") is not True or before.get("overview_app_grid") is not True or after.get("overview_open") is not False:
        raise ValueError("old Escape-close discriminator missing")
    a, b = before.get("rendered_frames"), after.get("rendered_frames")
    if type(a) is not int or type(b) is not int or a < 0 or b <= a:
        raise ValueError("old discriminator is stale")


def validate_assertions(lines, role):
    if role not in ("old", "fixed") or not isinstance(lines, list) or any(not isinstance(line, str) for line in lines):
        raise ValueError("invalid comparison role/assertions")
    expected = "G-GRID-DRAG-OLD-NEG pass" if role == "old" else "G-GRID-DRAG-CANCEL pass"
    forbidden = "G-GRID-DRAG-CANCEL pass" if role == "old" else "G-GRID-DRAG-OLD-NEG pass"
    if (any(" fail " in line or line.startswith(forbidden) for line in lines)
            or sum(line.startswith(expected + " ") for line in lines) != 1):
        raise ValueError("no exact discriminating gate pass")
