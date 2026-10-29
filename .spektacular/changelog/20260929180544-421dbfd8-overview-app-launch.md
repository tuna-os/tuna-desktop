---
created_date: "2026-09-29"
document_status: draft
---

# Overview app launch

## What was built

Overview search results are now actionable: pressing an app
result launches it through the shared launch tracker with
overview dismissal and the same starting feedback dock
launches show; pressing an action result runs it through the
matching path (launch or window activation). A launch that
fails keeps the overview open and marks the failed row
inline instead of vanishing.

## Why it matters

Typing a name and pressing it is the fastest way to start
anything. Results were previously informational only; now
the overview is a launcher with honest failure reporting,
reusing the dock's tested spawn path instead of growing a
second one.

## Deviations from the plan

None.
