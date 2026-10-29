---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Implementation test plan: xwayland-support

Manual procedures for the success metrics no automated test can cover
(headless CI has no X server). All three were executed 2026-09-30
against the real implementation; results recorded.

## Setup (all procedures)

- Xvfb :99 (`Xvfb :99 -screen 0 1280x800x24`), `DISPLAY=:99`.
- `cargo build -p roost-compositor --features xwayland`, then
  `./target/debug/roost-compositor --xwayland --shell-bin ./target/debug/roost-shell-host`.
- Headless X input needs explicit focus (Xvfb uses PointerRoot focus):
  `W=$(DISPLAY=:99 xdotool search --onlyvisible . | head -1)` then
  `DISPLAY=:99 xdotool windowfocus $W` before any `xdotool key/type`.
- Screenshots: `DISPLAY=:99 scrot <out.png>`.

## 1. Legacy app maps, focuses, takes input — PASS

- `DISPLAY=:0 xterm -title probe &`
- Expect `roost-compositor: X11 window N mapped (<title>)` in the log.
- Click the xterm, `xdotool type "echo FIXED-1"`, `xdotool key Return`.
- Pass: typed text + command output visible in a nested screenshot;
  xterm carries `_NET_WM_STATE_FOCUSED` on :0.
- Result 2026-09-30: PASS (screenshot + xwd pixel dump).

## 2. Exactly one server start; silence when native-only — PASS

- With `--xwayland`: `grep -c "xwayland: server started" <log>` must be 1.
- Native-only session (no `--xwayland`): `grep -c xwayland <log>` must be 0.
- Result 2026-09-30: PASS (1 and 0 respectively).

## 3. Overview and switcher list the legacy window — PARTIAL

- Switcher: hold Alt+Tab, screenshot mid-hold. Pass: popup lists one
  slot per mapped window with a selection highlight.
  Result: PASS for listing (two xterms → two slots + highlight).
  Note: slots render without titles by design (`SwitcherCanvas`
  draws id slots only, for native windows too) — "with its title"
  does not apply to the switcher UI.
- Overview: Super tap / hot corner / Activities click. Expected:
  fullscreen overview layer with window titles.
  Result 2026-09-30: NOT VERIFIED — the intent flips and the shell
  creates/commits a fullscreen `roost-shell-overview` layer (protocol
  traced) and key routing moves to overview search, but no overview
  pixels render (transparent canvas, shell-side gap). Tracked for the
  overview track; outside this plan's scope.

Who/when: re-run procedures 1–2 pre-release on any change to the
xwayland supervisor, window-manager ingest, input routing, or frame
production. Procedure 3 (overview) becomes runnable once the shell
paints the overview canvas.
