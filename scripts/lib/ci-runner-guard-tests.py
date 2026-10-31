#!/usr/bin/env python3
"""Fork pull requests never reach the self-hosted runners (docs/ci-runners.md).

Evaluates every `runs-on` expression in ci.yml for each event shape and runs
the runner image's job-started hook against the same events.
"""
import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.github/workflows/ci.yml'
HOOK = ROOT / '.github/runners/image/job-started.sh'
REPO = 'tuna-os/tuna-desktop'
FORK = 'someone/tuna-desktop'
SELF_HOSTED = ('tuna-ci-24.04', 'tuna-ci-26.04')
CONTEXT = {
    'vars.CI_RUNNERS': 'ci_runners',
    'github.event_name': 'event_name',
    'github.event.pull_request.head.repo.full_name': 'head_repo',
    'github.repository': 'repository',
}


def evaluate(expression, ci_runners, event_name, head_repo):
    """GitHub expression semantics for the subset used: ==, !=, &&, || return operands."""
    body = re.fullmatch(r'\$\{\{(.*)\}\}', expression.strip()).group(1)
    tokens = re.findall(r"'[^']*'|[A-Za-z_][\w.-]*|&&|\|\||==|!=|\(|\)|\S", body)
    python = []
    for token in tokens:
        if token in ('&&', '||'):
            python.append(' and ' if token == '&&' else ' or ')
        elif token in ('==', '!=', '(', ')') or token.startswith("'"):
            python.append(token)
        elif token in CONTEXT:
            python.append(CONTEXT[token])
        else:
            raise AssertionError(f'unexpected token {token!r} in runs-on expression')
    names = {'ci_runners': ci_runners, 'event_name': event_name, 'head_repo': head_repo, 'repository': REPO}
    return eval(''.join(python), {'__builtins__': {}}, names)  # tokens are whitelisted above


def runs_on_lines():
    return [line.split('runs-on:', 1)[1].strip() for line in WORKFLOW.read_text().splitlines()
            if line.strip().startswith('runs-on:')]


def expressions():
    found = {}
    for value in runs_on_lines():
        anchor = re.match(r'&(runner-\d+) (.*)', value)
        if anchor:
            found[anchor.group(1)] = anchor.group(2)
    return found


class RunsOnExpression(unittest.TestCase):
    def test_every_runs_on_is_guarded_alias_or_hosted_label(self):
        anchors = expressions()
        self.assertEqual(set(anchors), {'runner-24', 'runner-26'})
        for value in runs_on_lines():
            with self.subTest(runs_on=value):
                if value.startswith('*'):
                    self.assertIn(value[1:], anchors)
                elif not value.startswith('&'):
                    self.assertRegex(value, r'^ubuntu-[\w.]+$')
                    self.assertNotIn('tuna-ci', value)

    def test_dry_run_both_cases(self):
        for name, expression in expressions().items():
            hosted = 'ubuntu-latest' if name == 'runner-24' else 'ubuntu-26.04'
            own = 'tuna-ci-24.04' if name == 'runner-24' else 'tuna-ci-26.04'
            cases = [
                # (CI_RUNNERS, event, head repository) -> label
                ('', 'pull_request', REPO, own),
                ('', 'pull_request', FORK, hosted),
                ('self-hosted', 'pull_request', FORK, hosted),
                ('', 'push', None, own),
                ('', 'schedule', None, own),
                ('hosted', 'pull_request', REPO, hosted),
                ('hosted', 'push', None, hosted),
            ]
            for ci_runners, event, head, label in cases:
                with self.subTest(alias=name, ci_runners=ci_runners, event=event, head=head):
                    result = evaluate(expression, ci_runners, event, head)
                    print(f'{name}: CI_RUNNERS={ci_runners or "<unset>"} {event} head={head} -> {result}')
                    self.assertEqual(result, label)
                    if head == FORK:
                        self.assertNotIn(result, SELF_HOSTED)


class JobStartedHook(unittest.TestCase):
    def run_hook(self, repository, event, head=None):
        with tempfile.TemporaryDirectory() as tmp:
            payload = Path(tmp, 'event.json')
            payload.write_text(json.dumps({'pull_request': {'head': {'repo': {'full_name': head}}}} if head else {}))
            env = dict(os.environ, GITHUB_REPOSITORY=repository, GITHUB_EVENT_NAME=event,
                       GITHUB_EVENT_PATH=str(payload), GITHUB_REF='refs/test')
            return subprocess.run(['bash', str(HOOK)], env=env, capture_output=True, text=True)

    def test_admits_own_branches(self):
        for event, head in (('pull_request', REPO), ('push', None), ('schedule', None), ('workflow_dispatch', None)):
            with self.subTest(event=event):
                self.assertEqual(self.run_hook(REPO, event, head).returncode, 0)

    def test_refuses_forks_foreign_repositories_and_other_events(self):
        for repository, event, head in ((REPO, 'pull_request', FORK), (REPO, 'pull_request', None),
                                        (FORK, 'push', None), (REPO, 'pull_request_target', REPO),
                                        (REPO, 'issue_comment', None)):
            with self.subTest(repository=repository, event=event, head=head):
                result = self.run_hook(repository, event, head)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('refused this job', result.stderr)


if __name__ == '__main__':
    unittest.main(verbosity=2)
