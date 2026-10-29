---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929180510-b20ab21e-notification-presence
plan: 20260929180510-b20ab21e-notification-presence
---

# Notification presence in rust-wayland-desktop

App notifications now reach the user: banners with working
actions, an unread marker in the top bar, and a queue that
survives shell restarts — all from unmodified GNOME apps over
the standard notifications interface.

> Derived from project rust-wayland-desktop (plan store), spec/plan 20260929180510-b20ab21e-notification-presence. See the project-level record for the full feature.

## What changed in this repo

- New intake module serving the notifications bus name and
  translating wire calls into the notification center
  (`crates/shell-host/src/intake.rs`).
- Banner canvas paints real text with urgency/expanded states;
  reconcile binds banners to the layer surface with
  focus-free press routing (`overview.rs`, `panel.rs`).
- Bar tile carries the unread count in its paint key
  (`panel.rs`, `overview.rs`).
- Center gains atomic load/save of the unread queue under
  the XDG state dir (`notifications.rs`).

## Why

GNOME apps notify over a standard bus interface; serving it
in the shell is what makes those notifications visible with
working actions instead of vanishing.
