---
tags: [gsettings, gio, lifetime]
---

# GSettings subscriptions need retained objects

A change subscription needs its GSettings object to remain alive. Retain the object for the subscription’s lifetime; a temporary object dropped after connecting the callback does not provide a durable live-settings subscription.
