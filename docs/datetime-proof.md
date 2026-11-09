# Date & Time proof gates (#359)

Ten gates qualify GNOME Date & Time services on the shipped Marlin
image. They live in [`tuna-datetime-proof.py`](../scripts/lib/tuna-datetime-proof.py)
with fixture tests in
[`tuna-datetime-proof-tests.py`](../scripts/lib/tuna-datetime-proof-tests.py)
(the CI `check` job runs them). The Marlin VM lanes run the full
collection in the guest (`tuna-vm-lifecycle datetime`, recorded by
`scripts/tuna-vm-lane`), and the assertions reach the parity-ledger job
through the `marlin-vm` artifact (`lifecycle/datetime-evidence.json`
keeps the bundle).

## Gates

| Gate | What it proves | GNOME 51 reference |
|---|---|---|
| G-DATETIME-NTP | An NTP provider is active and the clock is synchronized (`timedatectl show` + `status` agree: `NTP=yes`, service `active`, synchronized `yes`; `chronyc tracking` too when chronyd serves). | — |
| G-DATETIME-TZ | The zone round-trips through timedate1 `SetTimezone(s, b)`, the call the Settings panel makes, and the original zone is restored and re-verified. | gnome-control-center 51.0 `panels/system/datetime/cc-datetime-page.c` `queue_set_timezone` |
| G-DATETIME-TZ-REJECT | Negative control: an invalid zone is refused and the zone is unchanged. | — |
| G-DATETIME-RTC | The RTC is kept in UTC (`LocalRTC=no`). No claim about RTC hardware accuracy; `SetLocalRTC` is never called. | — |
| G-DATETIME-CLOCK | The panel clock (an AT-SPI dump of `tuna-shell-gtk`, 24-hour or 12-hour text) shows the system time within 120 s. | — |
| G-DATETIME-CLOCK-TZ | Within 10 s of `SetTimezone` the panel clock shows the new zone's wall time and no longer the old one. | gnome-desktop 51.0 `libgnome-desktop/gnome-wall-clock.c` lines 77-85 and 449-486 (a monitor on `/etc/localtime` re-reads the zone) |
| G-DATETIME-MANUAL | Manual time as the panel sets it: `SetTime` is refused while NTP is on (why the Date & Time row is insensitive then); `SetNTP(false)` stops the provider; an absolute `SetTime` moves the clock; the real time (carried on the monotonic clock) and NTP are written back, and the provider's enablement is unchanged. | `cc-datetime-page.c` `queue_set_ntp`, `on_timezone_changed`, `on_permission_changed` |
| G-DATETIME-CLOCK-SET | Within 10 s of the manual `SetTime` the panel clock shows the new time. | `gnome-wall-clock.c` (timerfd cancelled on clock set) |
| G-DATETIME-POLKIT | The desktop user's own non-interactive `SetTimezone`, run from the user's systemd manager outside any logind session as GNOME launches apps, is allowed exactly when GNOME's rule grants it (member of the rule's privileged group) and otherwise refused with polkit's demand for interactive authentication; the zone is restored. | gnome-control-center 51.0 `panels/common/gnome-control-center.rules.in`, `panels/system/datetime/org.gnome.controlcenter.system.policy.in` (`datetime.configure` implies the timedate1 actions) |
| G-DATETIME-NTP-ABSENT | A stopped provider reads `NTP=no`, `CanNTP=yes`; a missing one (runtime mask) reads `CanNTP=no` and `SetNTP(true)` is refused, the state in which Settings hides Automatic Date & Time; the provider is restored. | `cc-datetime-page.c` `is_ntp_available`, `on_can_ntp_changed` |

A gate with missing inputs fails as unevaluable; missing evidence is
never a pass. A probe that fails halfway keeps its partial record, and
the gate's detail carries the probe error.

The published-flavor lane reads the installed `tuna-desktop` package version
with Marlin's native `pacman -Q` into `desktop_package_version` in that evidence
bundle. This is provenance, not a capability exemption: every gate records
its actual verdict, including **G-DATETIME-CLOCK-TZ** and
**G-DATETIME-CLOCK-SET**. Unsupported or unevaluable panel-follow results
remain failures even for known older packages. Current-build success does
not replace qualification of the published binary, and the original
measurements and partial failures remain in the evidence.

The known beta.2-2 package rebuilds immutable tag `v1.0.0-beta.2`, source
`1e6ce28f4d5dfd7918ac7340bd93454a9fcc211c`. Its `wall_clock.rs` lacks
the `/etc/localtime` monitor; #886's merge
`414e358eff9bbfc871637696b9382e6578f396ae` is later. A package-recipe revision
alone does not bring that newer desktop source into the published image.
A replacement release needs its own source/package/image binding and actual
panel-follow proof; changing a version allowlist is not a repair.

## Running

Unit tests (no VM, no privileges, no clock writes):

```sh
python3 scripts/lib/tuna-datetime-proof-tests.py
```

Full collection, as root on the guest with a desktop user logged in.
Every write probe restores what it changed (the NTP mask is
`--runtime`, so even an interrupted run cannot outlive a reboot):

```sh
python3 scripts/lib/tuna-datetime-proof.py collect --out datetime-evidence.json \
    --desktop-user tuna-test
python3 scripts/lib/tuna-datetime-proof.py check datetime-evidence.json \
    --assertions assertions.txt
```

`--read-only` skips every write probe (NTP, RTC and the panel only).
`check` prints one `<ID> pass|fail <detail>` line per gate, the shape
`scripts/tuna-ledger` reads, and exits 1 when any gate fails.

## Published-image evidence (2026-10-30)

Corral VM of `ghcr.io/tuna-os/marlin@sha256:ae6363c185015849701328fc38694f5ddcc0b4c804034a1d8d05577f1c70cf7d`
(marlin:tuna, version cbb3569; tuna-desktop 1.0.0beta.1-1, systemd
262.1, polkit 128, gnome-control-center 51.1, geoclue 2.8.2), retained in
[`marlin/2026-10-30-datetime-359/`](marlin/2026-10-30-datetime-359/).
This is the published image, so it runs main's shell, not this change.

- Gates, current script (`proof-run-*.txt`, `dt-*.json`): with
  `tuna-test` outside `wheel` and inside it, every gate passes except
  G-DATETIME-CLOCK-TZ: the panel clock kept the old zone for the whole
  10 s window (and, in `12-topbar-after-75s.png`, for over a minute)
  because the shell read the zone through jiff's five-minute system-zone
  cache. The shell now watches `/etc/localtime` as GnomeWallClock does
  (`crates/shell-gtk/src/wall_clock.rs`); the current-build lane proves it.
- Settings panel with real input: as a non-admin, "Some settings are
  locked" (`05-panel-nonadmin.png`); as an admin the rows unlock
  (`07-panel-admin.png`), choosing Auckland in Select Time Zone sets
  `Pacific/Auckland` (`tz-panel-auckland.txt`), turning Automatic Date &
  Time off stops and disables timesyncd (`ntp-off-panel.txt`), three
  clicks on the hour arrow move the clock 3 h and the top bar repaints
  at once (`18-hour-plus3.png`, `manual-time.txt`), and switching it
  back on resynchronizes (`ntp-on-restored.txt`).
- Polkit: matching GNOME's rule, a `wheel` member's non-interactive
  `SetTimezone` succeeds and a non-member's is refused
  (`polkit-*-noninteractive.txt`). Group changes reach polkit only after
  a new login, because polkit reads the caller's own group credentials
  (`polkit-subject-compare.txt`). The admin password dialog itself cannot
  authenticate on this image: the shell's agent runs
  `polkit-agent-helper-1` directly, polkit 128 ships it without setuid
  (socket-activated instead), and the dialog fails and respawns the
  helper in a tight loop (`06b-unlock-after-password.png`,
  `polkit-nonadmin-interactive.txt`);
  [#884](https://github.com/tuna-os/tuna-desktop/issues/884).
- Reboot: the zone chosen in Settings survives a reboot, RTC stays UTC,
  and the session's clock starts in the new zone (`reboot.txt`,
  `14-session-after-reboot.png`).
- Missing service: with timesyncd masked, Settings hides Automatic Date
  & Time and makes Date & Time editable (`21-masked-panel-reopened.png`);
  timedated does not signal the change, so an open panel keeps its rows
  until reopened, as in GNOME.
- Automatic time zone is unmeasured: the published session starts no
  settings-daemon plugins (`geoclue-auto-tz.txt`), and gsd-datetime
  started by hand cannot get a GeoClue client (`Failed to connect to
  GeoClue2 service: Timeout was reached`, `AvailableAccuracyLevel 0`,
  `geoclue-gsd-datetime-probe.txt`); the VM has no Wi-Fi, modem or GPS
  source. Hardware with a location source was not available.

## Scope limits

- The `timedatectl`/`chronyc` fixtures under `scripts/lib/fixtures/`
  are parser-shape captures, not Marlin evidence.
- RTC hardware accuracy is out of scope: the gate checks the UTC
  policy only.
- Automatic time zone through Geoclue is unqualified (above).
- Reboot persistence is Corral evidence only; the VM lane gates run
  within one boot.
