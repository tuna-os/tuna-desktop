# Installed portal failures: locked-session denial, not missing capture support

## Subject and limits

Corral VM `tuna-q-isg-yf1`, `karnataka`, 4 vCPU, 6 GiB, 40 GiB,
bochs-drm `Virtual-1` 1280×800. Installed image:
`ghcr.io/tuna-os/yellowfin@sha256:64e7a29a9d832c0d021d3023ee825c07d37676d722dca59b31a2dfe492e5d0b0`;
booted amd64 manifest
`sha256:1ca595e2ab421861a51e293e99a15481553be2f3772f8f39c9617bda93d84fb6`.
Real password login through stock GDM created seat0 Wayland session 4,
`Service=gdm-password`, running Tuna's DRM compositor and GTK shell.

The published product payload stayed **unchanged**: no candidate binaries,
package installation, portal substitution, app-ID spoofing, capture permission
bypass, idle-key change or lock-setting change. Only native test requester and
AT-SPI helper sources were put in `/run/user/1000/isg`. Public fixture password
`tuna` was entered through Corral's real console. [Provenance and complete
session journal](installed-provenance-journal.txt), [file hashes](receipt.json)
and [VM/VMI/PVC deletion plus empty owned inventory](cleanup.json) are retained.

Installed packages: Tuna `1.0.0~beta.2-2.el10`, frontend portal `1.20.0-2.el10`,
GNOME backend `47.3-1.el10`, GTK backend `1.15.3-1.el10`, PipeWire `1.6.9-1.el10`.
Compositor SHA-256 `addd99478870b9d0a73f2a4f4c79a5b71878952588a12fbb62f25a81bfd18fa6`;
shell `dcf1133a966ea089d3b1d727ccdf77952d146c55a22a43242a450042fb6ebd44`.
These match the earlier refreshed-image receipt. This is not a qualification of
other digests or an assertion that all v1 gates pass.

## 1. Screenshot Response 0 with empty URI

The earlier receipt is preserved unchanged in [original-refreshed](original-refreshed/manifest.json).
Its [request-time frame](original-refreshed/portal-screenshot-prompt.png) and
[failed-step frame](original-refreshed/05-portal-screenshot.png) are entirely
black, not a visibly usable Allow dialog. The AT-SPI tree alone did not establish
that the compositor was unlocked.

On the **same unchanged published image**, real pointer activity followed by
`ScreenSaver.GetActive=false`, a native persistent frontend requester and
actual AT-SPI Allow delivered:

- [Successful response](guest/shot2.response.json): Response 0, absolute local
  URI `file:///var/home/tuna-test/Pictures/Screenshots/Screenshot%20From%202026-10-31%2020-39-11.png`.
- [Real 1280×800 varied-pixel image](guest/shot2.png), SHA-256
  `f9dc77c6f9d3bf1a243d1c02f04321567afac0f84daf5fc85f2e30ecdc36cebe`.
- [Visible Allow frame](03-unlocked-screenshot2-prompt.png) and
  [unlocked-before/after, requester exit 0 and actual consent](unlocked-screenshot2-result.txt).
- [Raw D-Bus trace](guest/bus.txt), lines 699–755: frontend -> GNOME Screenshot
  backend -> Tuna `org.gnome.Shell.Screenshot.Screenshot(false,true,"Screenshot")`;
  Tuna returns `(true, absolute filename)`; GNOME returns the file URI; the
  frontend emits Response 0 containing it. This traces the actual provider,
  rather than assuming that Tuna implements `org.freedesktop.impl.portal.Screenshot`.

The compositor has `HOME=/var/home/tuna-test` and no exported
`XDG_PICTURES_DIR`; `user-dirs.dirs` names `$HOME/Pictures`. Tuna's fallback
uses `$HOME/Pictures/Screenshots`. The directory was created with owner
`tuna-test:tuna-test`, mode 755, and the saved file hash matches the copied PNG.
The retained [environment and directory diagnostics](unlocked-screenshot-diag.txt)
rule out a missing XDG directory or write permission failure for this subject.

A deliberate real Super+L then reproduced **exactly** the reported failure:
[GetActive=true before/after; Response 0, `uri=""`; requester exit 0 in its locked
negative mode](locked-screenshot-result.txt). The [before/after image-hash
receipt](guest/locked-shot.unchanged-images.json) records no new or changed PNG.
The frozen [native requester](harness/tuna-portal-screenshot-client.py), lines
56–65, already explicitly expected this locked result.

Causal boundary: Tuna's Screenshot service refuses locked capture with
`(false, "")`; GNOME's noninteractive backend ignores the boolean in
`screenshot_done`, calls `maybe_show_screenshot` with the empty filename and
completes `GTK_RESPONSE_OK`. `g_filename_to_uri("")` cannot produce a local URI,
so its response uses an empty string. In GNOME 51, this is visible in
[screenshotdialog.c](https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/blob/51.0/src/screenshotdialog.c)
(`screenshot_done`, `maybe_show_screenshot`) and
[screenshot.c](https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/blob/51.0/src/screenshot.c)
(`screenshot_dialog_done`, `send_response`). GNOME Shell 51 itself uses the
same `(false, '')` failure tuple in
[its ScreenshotAsync exception path](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/screenshot.js).
This comparison concerns the failure tuple/backend translation, not a claim
that a GNOME lock-screen capture was exercised in this VM.

One earlier **unlocked diagnostic attempt** also remains a failure:
[Response 2](guest/shot.response.json), [requester log](guest/shot.log), and
[journal](unlocked-screenshot-diag.txt) report `Failed to show access dialog:
Timeout was reached` after host inspection delayed the consent action. It is
not relabelled a product pass. The next diagnostic changed only orchestration:
actual consent six seconds after launch, within the original deadlines.

## 2. ScreenCast Start Response 2 after Share on Virtual-1

The original [request-time frame](original-refreshed/portal-screencast-prompt.png)
is visibly Tuna's **lock password prompt**, and its hash equals
[the explicit lock-step frame](original-refreshed/08-lock.png). An accessible
Share widget behind that curtain is not visible user consent.

With `GetActive=false` before and after, actual AT-SPI Share for Virtual-1 on
the same unchanged image produced:

- [Response 0 with real node 58](guest/cast.response.json).
- [Visible picker](04-unlocked-screencast-prompt.png), [actual accessible
  controls](guest/cast-consent.json), and [native requester result](unlocked-cast-result.txt).
- [Actual GStreamer-decoded PipeWire frame](guest/cast.png), 1280×800 with
  visibly varied desktop pixels; no static fixture image.
- [Raw bus trace](guest/cast-bus.txt), lines 343–504: GNOME backend creates
  Tuna's Mutter session, calls `RecordMonitor("Virtual-1", cursor-mode=0)`,
  starts it, receives `PipeWireStreamAdded(58)`, then returns frontend Start
  Response 0. The client opens the real frontend PipeWire FD and decodes it.
- [Close removal receipt](guest/cast.log): caller isolation passed and explicit
  Session.Close removed node 58 in 0.013 seconds. **This first run is Close
  withdrawal, not lock-triggered withdrawal proof.**

While deliberately locked, actual AT-SPI Share reproduced
[Start Response 2](guest/locked-cast.response.json) with
[GetActive=true before/after](locked-cast-result.txt).
[Raw locked-session trace](guest/locked-bus.txt), lines 240–246, shows the exact
cause: GNOME calls Tuna's Mutter `CreateSession`; Tuna returns
`org.freedesktop.DBus.Error.AccessDenied: session locked`; the GNOME backend
returns response 2. The failure is **before PipeWire node creation**, not an
installed-session PipeWire connection/format failure. The locked session also
refuses window enumeration. No product gate was relaxed to make it share.

## Harness correction and installed proof after correction

The journey now sends harmless real pointer motion around long availability
probes and before capture requests. It records GetActive before request and
after response; a locked/unreadable session is an explicit precondition
failure, not a provider failure. The external-capture consent helper checks
GetActive immediately before dispatching Allow/Share. AT-SPI `STATE_SHOWING`
without that check previously allowed actions on dialogs hidden by the lock.
It never unlocks a session or changes its idle/lock configuration.

A second causal timing defect was that host response/log/hash/PNG transfers
and step screenshot/journal collection consumed the native ScreenCast client's
unchanged 30-second active-grant hold before the lock marker was sent. The
journey now arms the observer and sends actual noVNC Super+L immediately after
observing an active grant and confirming the session is unlocked. It defers
those transfers until after lock withdrawal. **The 30-second hold and
existing two-second withdrawal deadline are unchanged.** No Session.Close
substitutes for lock withdrawal.

The actual corrected journey methods were then exercised against the still
unchanged installed Yellowfin session. [Frozen harness and
manifest](corrected-harness/manifest.json) identify SHA-256
`2e1b77bebfa27c5077b197c8e47a4d81f5c2faf81adeee9c03d9a24b17263e01`:

- ScreenCast step passes with actual Share, helper-reported unlocked state,
  and GetActive=false before/after Start.
- Real Super+L, with observer armed before the key, removes node 59 in
  **0.147 seconds**. The native client exits 0, and the retained varied
  [1280×800 frame](corrected-harness/portal-screencast-frame.png) matches
  guest SHA-256 `45bc0deb8d4e3ce4fb270f90c6477be9dd65448c20e415fb1572afb7ac6a0e43`.
- The lock step still **fails** its separate `LockedHint=yes` check: observed
  `no`, correctly retaining the published beta2 omission of main's later #849.
  It is not a full lock gate pass. Correct-password unlock passes.

The focused run exercised only capture, lock and unlock, not a complete
variant journey. An additional [actual repaired-helper result](fixed-guard-installed-result.txt)
records node 60, unlocked consent and Close removal in 0.01 seconds; it is
not substituted for the subsequent lock-triggered evidence.

Regression coverage in `scripts/tests/test_corral_journey_portals.py` checks
locked/unreadable preconditions, both request launch sites, real pointer
activity without clicks/unlock, an AT-SPI-showing Share hidden behind a lock,
fail-closed lock-query errors, and arm/key ordering before transfers and step
collection (including arm failure, without a retry). These unit tests were
added but not locally run by this worker; the integration evidence above is
from real installed native services and console input, not those test doubles.

No portal product patch is warranted by these observations. Keep capture
security unchanged. The independent logind hint and notification-interface
omissions require the already-main fixes to be included in a qualified
published source/package; neither is repaired by reinterpreting these portal
receipts as an all-gates pass.
