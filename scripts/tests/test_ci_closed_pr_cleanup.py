#!/usr/bin/env python3
"""Offline safety tests for the closed-PR queue cleanup (no API mutations)."""
import copy
import importlib.machinery
import importlib.util
from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
loader = importlib.machinery.SourceFileLoader("closed_pr_cleanup", str(SCRIPTS / "ci-cleanup-closed-pr-runs"))
spec = importlib.util.spec_from_loader(loader.name, loader)
cleanup = importlib.util.module_from_spec(spec)
loader.exec_module(cleanup)
REPO = "tuna-os/tuna-desktop"


def run(run_id=1, numbers=(42,), **fields):
    result = {"id": run_id, "event": "pull_request", "status": "queued", "head_sha": "a" * 40,
              "pull_requests": [{"number": n, "url": f"https://api.github.com/repos/{REPO}/pulls/{n}"}
                                for n in numbers]}
    result.update(fields)
    return result


class API:
    def __init__(self, runs, states=None):
        self.runs = runs
        self.states = states or {42: "closed"}
        self.current = {item["id"]: copy.deepcopy(item) for item in runs}
        self.pr_reads = {}
        self.fail = None
        self.cancelled = []

    def get(self, path):
        if path == self.fail:
            raise RuntimeError("API unavailable")
        if path.startswith("actions/runs?"):
            return {"total_count": len(self.runs), "workflow_runs": self.runs}
        if path.startswith("pulls/"):
            number = int(path.split("/")[-1])
            index = self.pr_reads.get(number, 0)
            self.pr_reads[number] = index + 1
            state = self.states[number]
            if isinstance(state, list):
                state = state[min(index, len(state) - 1)]
            return {"number": number, "state": state}
        return self.current[int(path.split("/")[-1])]

    def sweep(self, apply=True):
        cleanup.cleanup(REPO, self.get, self.cancelled.append, apply=apply)


class ClosedPRCleanupTests(unittest.TestCase):
    def test_closed_queued_run_is_cancelled(self):
        api = API([run()])
        api.sweep()
        self.assertEqual(api.cancelled, [1])

    def test_dry_run_never_cancels(self):
        api = API([run()])
        api.sweep(apply=False)
        self.assertEqual(api.cancelled, [])

    def test_current_open_head_is_untouched(self):
        api = API([run()], {42: "open"})
        api.sweep()
        self.assertEqual(api.cancelled, [])

    def test_no_age_branch_or_head_fallback(self):
        for fields in ({"pull_requests": []}, {"pull_requests": None},
                       {"pull_requests": [{"number": 42, "url": "https://api.github.com/repos/other/repo/pulls/42"}]}):
            with self.subTest(fields=fields):
                api = API([run(head_branch="old-branch", created_at="2000-01-01T00:00:00Z", **fields)])
                api.sweep()
                self.assertEqual(api.cancelled, [])

    def test_shared_run_requires_every_associated_pr_closed(self):
        api = API([run(numbers=(42, 43))], {42: "closed", 43: "open"})
        api.sweep()
        self.assertEqual(api.cancelled, [])
        api = API([run(numbers=(42, 43))], {42: "closed", 43: "closed"})
        api.sweep()
        self.assertEqual(api.cancelled, [1])

    def test_running_scheduled_push_and_dispatch_are_untouched(self):
        for fields in ({"status": "in_progress"}, {"event": "schedule"},
                       {"event": "push"}, {"event": "workflow_dispatch"}):
            with self.subTest(fields=fields):
                api = API([run(**fields)])
                api.sweep()
                self.assertEqual(api.cancelled, [])

    def test_run_start_reopen_and_identity_changes_are_rechecked(self):
        for fields in ({"status": "in_progress"}, {"head_sha": "b" * 40},
                       {"pull_requests": []}):
            with self.subTest(fields=fields):
                api = API([run()])
                api.current[1].update(fields)
                api.sweep()
                self.assertEqual(api.cancelled, [])
        api = API([run()], {42: ["closed", "open"]})
        api.sweep()
        self.assertEqual(api.cancelled, [])

    def test_preflight_api_failure_cancels_nothing(self):
        api = API([run(1), run(2, numbers=(43,))], {42: "closed", 43: "closed"})
        api.fail = "pulls/43"
        with self.assertRaises(RuntimeError):
            api.sweep()
        self.assertEqual(api.cancelled, [])

    def test_invalid_pr_response_cancels_nothing(self):
        api = API([run()], {42: "unknown"})
        with self.assertRaises(ValueError):
            api.sweep()
        self.assertEqual(api.cancelled, [])

    def test_listing_failure_or_incomplete_listing_cancels_nothing(self):
        for response in ({"total_count": 1001, "workflow_runs": []},
                         {"total_count": 1, "workflow_runs": []},
                         {"total_count": 2, "workflow_runs": [run(), run()]},
                         {"workflow_runs": [run()]}):
            with self.subTest(response=response):
                cancelled = []
                with self.assertRaises(ValueError):
                    cleanup.cleanup(REPO, lambda path: response, cancelled.append, apply=True)
                self.assertEqual(cancelled, [])
        api = API([run()])
        api.fail = "actions/runs?event=pull_request&status=queued&per_page=100&page=1"
        with self.assertRaises(RuntimeError):
            api.sweep()
        self.assertEqual(api.cancelled, [])

    def test_later_page_failure_aborts_before_mutation(self):
        cancelled = []

        def get(path):
            if "page=1" in path:
                return {"total_count": 101, "workflow_runs": [run(i) for i in range(1, 101)]}
            raise RuntimeError("second page failed")

        with self.assertRaises(RuntimeError):
            cleanup.cleanup(REPO, get, cancelled.append, apply=True)
        self.assertEqual(cancelled, [])

    def test_multiple_pages_are_fully_collected(self):
        def get(path):
            if path.endswith("page=1"):
                return {"total_count": 101, "workflow_runs": [run(i) for i in range(1, 101)]}
            return {"total_count": 101, "workflow_runs": [run(101)]}

        self.assertEqual(len(cleanup.list_queued(get)), 101)


if __name__ == "__main__":
    unittest.main()
