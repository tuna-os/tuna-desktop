#!/usr/bin/env bash
# ACTIONS_RUNNER_HOOK_JOB_STARTED: runs before every job on this runner.
# The workflow `runs-on` expressions keep fork pull requests on GitHub-hosted
# runners, but a fork can edit those expressions in its own pull request.
# This hook lives in the runner image, out of reach of any workflow, and
# fails the job unless it comes from tuna-os/tuna-desktop's own branches.
set -euo pipefail

repo=tuna-os/tuna-desktop
deny() {
  echo "::error::tuna-ci self-hosted runner refused this job: $*" >&2
  exit 1
}

[[ "${GITHUB_REPOSITORY:-}" == "$repo" ]] || deny "repository '${GITHUB_REPOSITORY:-}' is not $repo"
case "${GITHUB_EVENT_NAME:-}" in
  push | workflow_dispatch | schedule) ;;
  pull_request)
    head=$(jq -r '.pull_request.head.repo.full_name // empty' "$GITHUB_EVENT_PATH")
    [[ "$head" == "$repo" ]] || deny "pull request head repository '$head' is a fork"
    ;;
  *) deny "event '${GITHUB_EVENT_NAME:-}' is not allowed" ;;
esac
echo "tuna-ci runner admitted ${GITHUB_EVENT_NAME} from ${GITHUB_REPOSITORY}@${GITHUB_REF:-}"
