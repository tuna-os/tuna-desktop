---
created_date: "2026-10-09"
document_status: draft
project: rust-wayland-desktop
spec: 20261009134949-7190f8bf-tuna-desktop-v1-definition
plan: 20261009134949-7190f8bf-tuna-desktop-v1-definition
---

# Victory signal for the v1 checklist

> Derived from project rust-wayland-desktop, spec/plan 20261009134949-7190f8bf-tuna-desktop-v1-definition. See the project-level record for the full feature.

Tuna Desktop's v1 checklist now reports live numbers. A nightly job
publishes how many parity checks pass, how many gaps stand open, and
which verdicts are still waiting on their lanes — so anyone can see
exactly what remains before v1 without asking.

What changed in this repo: a new nightly workflow and signal script
(`scripts/victory-signal.py`, `.github/workflows/victory-checks.yml`),
tests for the script wired into the check job
(`scripts/lib/victory-signal-tests.py`, one `ci.yml` line).

Why: the v1 bar is "every tracked capability verified, zero open
gaps, numbers beating GNOME" — this makes the first two visible
every night.
