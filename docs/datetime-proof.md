# Date & Time proof gates (#359)

Five PROOF-ONLY gates qualify GNOME Date & Time services on the shipped
Marlin image. They live in [`tuna-datetime-proof.py`](../scripts/lib/tuna-datetime-proof.py)
with fixture tests in
[`tuna-datetime-proof-tests.py`](../scripts/lib/tuna-datetime-proof-tests.py);
no product code is touched.

## Gates

| Gate | What it proves | Inputs |
|---|---|---|
| G-DATETIME-NTP | An NTP service is present, active, and the system clock is synchronized (`timedatectl show` + `status` agree: `NTP=yes`, service `active`, synchronized `yes`). When `chronyd` is the provider, `chronyc tracking` must also parse (stratum, leap status recorded). | `timedatectl`, `systemctl is-active`, `chronyc` |
| G-DATETIME-TZ | The timezone round-trips through timedate1 `SetTimezone` — the same bus method GNOME Settings' Date & Time panel calls (`org.freedesktop.timedate1`, `/org/freedesktop/timedate1`, `SetTimezone(s, b)`) — and the original zone is restored and re-verified afterwards. | privileged `busctl` on the guest |
| G-DATETIME-TZ-REJECT | Negative control: `SetTimezone` with an invalid zone fails and the zone is unchanged. | privileged `busctl` on the guest |
| G-DATETIME-RTC | The RTC is kept in UTC (`LocalRTC=no`). The collector never issues `SetTime`, `SetLocalRTC`, or `SetNTP`, and the gate makes no claim about RTC hardware accuracy. | `timedatectl show`, `/etc/localtime`, `/etc/adjtime` |
| G-DATETIME-CLOCK | The panel clock label (from a `tuna-a11y-dump.py` dump of the shell, the same source as G-SETTINGS-CLOCK) shows the system time within 120 s, resolving the label's wall time in the system's own zone. | a11y dump + epoch captured with it |

## Running

Unit tests (no VM, no privileges, no clock writes):

```sh
python3 scripts/lib/tuna-datetime-proof-tests.py
```

Read-only collection on any machine (NTP + RTC + optional panel gates;
proves the collector runs, not Marlin behavior):

```sh
python3 scripts/lib/tuna-datetime-proof.py collect --read-only \
    --a11y-dump a11y-clock.json --system-epoch "$(date +%s)" \
    --out datetime-evidence.json
python3 scripts/lib/tuna-datetime-proof.py check datetime-evidence.json
```

Full collection runs **inside the Marlin guest** (the VM lane guest agent
runs privileged, so `SetTimezone` with interactive=false needs no polkit
prompt) and always restores the original timezone, even when a
verification step fails:

```sh
python3 scripts/lib/tuna-datetime-proof.py collect --out datetime-evidence.json
python3 scripts/lib/tuna-datetime-proof.py check datetime-evidence.json \
    --assertions assertions.txt
```

`check` prints one `<ID> pass|fail <detail>` line per gate — the shape
`scripts/tuna-ledger` consumes — and exits 1 when any gate fails.
A gate with missing inputs fails as unevaluable; missing evidence is
never a pass.

## Evidence bundle

`collect` writes JSON with `ntp` (parsed + raw `show`/`status`,
provider unit states, timesync/chrony detail), `rtc` (policy fields),
`timezone_round_trip` (original → target → observed → revert verified),
`timezone_rejection` (exit code, before/after zone), and optionally
`panel_clock` (a11y nodes, captured epoch, zone, tolerance).

## Scope limits

- The `timedatectl`/`chronyc` fixtures under `scripts/lib/fixtures/`
  are parser-shape captures, not Marlin evidence.
- Geoclue automatic time zone and manual time setting (`SetTime`)
  remain unqualified; the collector deliberately cannot set the clock.
- RTC hardware accuracy is out of scope: the gate checks the UTC
  policy only.
- CI wiring is still needed: run the unit tests in the `check` job
  (`python3 scripts/lib/tuna-datetime-proof-tests.py`) and, once a
  guest-side step exists, full `collect`+`check` on the Marlin VM with
  its assertions fed to the parity-ledger job.
