---
tags: [smithay, rendering, geometry, wayland]
---

# Refresh Smithay window bounds before preview scaling

Smithay Window geometry is cached. Call `Window::on_commit()` for the committed surface before calculating preview bounds and scale. Stale bounds can make the same client render differently in the overview and the desktop.
