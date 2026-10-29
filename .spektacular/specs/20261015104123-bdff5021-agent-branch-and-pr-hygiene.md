---
created_date: "2026-10-15"
document_status: final
closed_date: "2026-10-15"
---

# Feature: 20261015104123-bdff5021-agent-branch-and-pr-hygiene

## Overview

Parallel coding agents produce work faster than it is reviewed and merged, so capability work accumulates in draft PRs and branch chains that drift from main and never land. This spec sets rules so agent work converges on main: one owner PR per capability, bounded draft lifetime, and a regular triage that merges, consolidates or closes.


## Requirements

- [ ] **One PR per capability issue**
  Each epic child issue has at most one open PR; follow-ups push to it instead of opening successor/union branches.
- [ ] **Drafts expire**
  A draft PR with no new commits for 7 days is triaged: rebased and promoted, consolidated, or closed with a recorded reason.
- [ ] **Green before ready**
  A PR is marked ready only with green required checks on its exact head.
- [ ] **Branch cleanup**
  Merged PR branches are deleted; closed PR branches are deleted only after the closing comment names where the unmerged commit is kept; local agent worktrees are removed when their PR closes.
- [ ] **Visible queue**
  Maintainers see one list of open capability PRs with status, owner and blocker.


## Constraints

- Closing a PR never deletes unmerged work without a pointer (closing comment names the branch/commit kept).
- Policy applies to human and agent contributors alike.
- Successor/union branches must not be opened for a capability that already has an open PR.


## Acceptance Criteria

- [ ] **No duplicate PRs per issue**
  A check reports zero issues with more than one open PR.
- [ ] **Stale drafts handled**
  No draft PR with no new commits for more than 7 days exists after a triage run.
- [ ] **Queue doc exists**
  CONTRIBUTING.md documents the rules and the triage cadence.


## Technical Approach

- A scheduled workflow lists open PRs per linked issue and stale drafts and posts a triage report issue.
- Document the rules, including the successor/union branch prohibition, in AGENTS.md/CONTRIBUTING.md.


## Success Metrics

- No success metrics defined beyond the acceptance criteria.


## Non-Goals

- Changing review requirements or CI gates themselves.
