---
created_date: "2026-09-30"
document_status: draft
---

# Overview canvas paint — project record

## What was built

The Roost overview now always paints. Pressing Super shows window
blocks, the selection border, and favorites in every session mode,
after output changes, and with no windows open. The work added five
live regression tests (paint on open, panel-only iteration, resize
repaint, empty-state commit, typing during paint) and one production
fix: a shared shell-surface update pass now runs in both the
control-paced loop and the pure-panel fallback loop.

## Why it matters

The overview is the front door to app launch and keyboard navigation;
operating it while it shows a blank canvas meant search results and
selection moved on something invisible. What the shell shows now
matches what it will activate.

## Deviations from the plan

The first regression-test draft failed as a test artifact (single
update pass before the configure arrived) rather than a production
bug; it was corrected to drive the production loop shape before
diagnosis. Diagnosis then showed the control path already painted and
isolated the real defect to the panel-only fallback loop. No
compositor changes; the canvas painter is untouched.
