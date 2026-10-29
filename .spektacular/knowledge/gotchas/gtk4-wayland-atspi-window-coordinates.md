---
tags: [gtk4, at-spi, wayland, graphical-tests]
---

# Use AT-SPI WINDOW coordinates in GTK4 Wayland tests

GTK4 Wayland can report zero SCREEN/DESKTOP coordinates. Use AT-SPI WINDOW bounds and add the compositor’s actual surface origin before sending pointer input. Re-measure the origin when the surface moves; use the reported button role variants when locating controls.
