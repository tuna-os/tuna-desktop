---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929180544-421dbfd8-overview-app-launch
plan: 20260929180544-421dbfd8-overview-app-launch
---

# Overview app launch in rust-wayland-desktop

Overview search results now launch: press an app to start it
with dock-style starting feedback, press an action to run it,
and failed launches report inline with the overview kept open.

> Derived from project rust-wayland-desktop (plan store), spec/plan 20260929180544-421dbfd8-overview-app-launch. See the project-level record for the full feature.

## What changed in this repo

- New press router mapping overview results to runnable
  actions in the dock-press shape (`overview.rs`).
- Host wiring: launch through the shared tracker with
  overview dismissal on success, inline failure state on
  spawn error (`panel.rs`).

## Why

The overview is the fastest way to start anything; results
must act, not just inform, while sharing the dock's tested
spawn path.
