---
created_date: "2026-09-29"
document_status: draft
---

# Notification presence

## What was built

Roost now serves the standard desktop notifications interface
from the shell host, so unmodified GNOME apps can notify the
user. A new intake module translates Notify, Close, and
capability queries into the existing notification-center model
and routes action and close signals back; banners with real
summary/body text paint on the existing banner layer surface
with press handling that never steals focus; a bar marker
shows unread presence and repaints within one tick; and the
unread queue persists across shell restarts in a versioned
state file. A ten-thousand-notification flood stays bounded
with newest kept.

## Why it matters

Notifications previously vanished: the center model existed
but nothing fed it and nothing showed it. With intake, paint,
marker, and durability wired, GNOME apps reach the user with
working actions while the shell stays toolkit-free and the
event loop unblocked.

## Deviations from the plan

None.
