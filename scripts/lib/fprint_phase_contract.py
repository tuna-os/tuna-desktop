import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request

REPO = "tuna-os/tuna-desktop"
WORKFLOW = ".github/workflows/ci.yml"
JOB = "fprint-phase-proof"


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



def select_artifacts(messages, root, target):
    root, target = Path(root).resolve(), Path(target).resolve()
    roles = {"tuna_compositor": ("lib", True, "lib.rs"),
             "tuna-compositor": ("bin", False, "main.rs")}
    selected = {}
    for m in messages:
        if m.get("reason") != "compiler-artifact" or not m.get("executable"):
            continue
        name = m.get("target", {}).get("name")
        if name not in roles:
            continue
        kind, is_test, filename = roles[name]
        src = root / "crates/compositor/src" / filename
        exe = Path(m["executable"])
        prefix = "path+" + (root / "crates/compositor").as_uri() + "#tuna-compositor@"
        if (m["target"].get("kind") != [kind] or m.get("profile", {}).get("test") is not is_test
                or m["target"].get("src_path") != str(src)
                or not re.fullmatch(re.escape(prefix) + r"[0-9]+\.[0-9]+\.[0-9]+", m.get("package_id", ""))
                or exe.is_symlink() or not exe.is_file() or not exe.resolve().is_relative_to(target)):
            raise ValueError("wrong compositor build role")
        rec = {"name": name, "kind": kind, "test": is_test, "source_path": str(src),
               "cargo_executable": str(exe.resolve()), "sha256": digest(exe)}
        if name in selected and selected[name] != rec:
            raise ValueError("ambiguous compositor role")
        selected[name] = rec
    if set(selected) != set(roles):
        raise ValueError("missing compositor test/bin roles")
    return selected


def phase_vectors(output, nonce, pid, expected):
    if not re.fullmatch(r"[0-9a-f]{32}", nonce) or type(pid) is not int or pid <= 0:
        raise ValueError("invalid child binding")
    if len(output.encode()) > 1024 * 1024:
        raise ValueError("phase output budget exceeded")
    if len(re.findall(r"^test result: ok\. 1 passed; 0 failed; 0 ignored;", output, re.M)) != 1:
        raise ValueError("exact actual one-passed zero-ignored execution required")
    vectors = {}
    for line in output.splitlines():
        if "FPRINT-PHASE" not in line:
            continue
        # libtest may prefix test name before our contiguous stderr record.
        marker = line.find("FPRINT-PHASE ")
        if marker < 0:
            raise ValueError("malformed phase marker")
        record = json.loads(line[marker + len("FPRINT-PHASE "):])
        phase = record.get("phase")
        if (phase not in expected or phase in vectors or record.get("nonce") != nonce
                or type(record.get("pid")) is not int or record["pid"] != pid
                or record.get("driver_exit") != "unobserved"):
            raise ValueError("phase binding/duplicate/scope mismatch")
        vectors[phase] = record
    if set(vectors) != set(expected):
        raise ValueError("required measured phases absent")
    return vectors


def positive(vectors):
    if set(vectors) != {"A1", "A2", "B"}:
        raise ValueError("all distinct measured phases required")
    def integer(m, key, minimum):
        if type(m.get(key)) is not int or m[key] < minimum:
            raise ValueError("missing/invalid measured counter " + key)
    def flags(m, keys):
        if any(m.get(k) is not True for k in keys):
            raise ValueError("observed lifecycle assertion missing")
    a = vectors["A1"]["measurements"]
    if a.get("scope") != "synthetic_socketpair": raise ValueError("A1 scope")
    for k in ("capacity", "outq"): integer(a, k, 1)
    flags(a, ("eagain", "send_parked", "shutdown_released_error", "sender_joined"))
    a = vectors["A2"]["measurements"]
    if a.get("scope") != "synthetic_zbus_signal": raise ValueError("A2 scope")
    for k in ("capacity", "outq", "d2c", "inq"): integer(a, k, 1)
    integer(a, "c2d", 4096)
    if a["outq"] < a["capacity"] or a["c2d"] >= 1024 * 1024: raise ValueError("A2 not saturated/frozen")
    flags(a, ("send_parked", "close_parked", "shutdown_released_error", "sender_joined", "closer_joined"))
    b = vectors["B"]["measurements"]
    if b.get("scope") != "public_verify" or b.get("branch") != "pending":
        raise ValueError("public non-fill is unit-pass but NOT pending-output qualification")
    for k, minimum in (("inq",1),("c2d",256),("d2c",1),("elapsed_ms",0),("claims",0)):
        integer(b,k,minimum)
    if b["elapsed_ms"] >= 15000 or b["claims"] > 1 or b.get("outcome") not in ("TimedOut","Error","NoDevice","NotEnrolled"):
        raise ValueError("public bounded denial/claim evidence absent")
    flags(b,("peer_live_at_retirement","claim_free_before_teardown"))
