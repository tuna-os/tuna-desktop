# Project Bluefin AT-SPI scope matrix

Tracks [#475](https://github.com/tuna-os/tuna-desktop/issues/475). This is an
**inventory**, not a result: no upstream scenario has been run against the
Tuna Desktop session, and nothing here counts as desktop compatibility. #397, #432
and #352 stay open until their full GNOME app, Settings and AT-SPI acceptance
is met.

[`bluefin-atspi-scope.json`](bluefin-atspi-scope.json) pins
`projectbluefin/testsuite` at `da6a487ec6783490f042e1cad6841f54caf835f0` and
its qecore 4.19.3, behave 1.3.3, dogtail 2.0.3 and python-uinput 1.0.1
dependencies (from the upstream `gnome-e2e` action at that revision). It
lists all 29 scenarios of the Files, Settings, accessibility and Orca
features with one disposition and reason each:

| Disposition | Meaning |
| --- | --- |
| `portable` | Real app interaction over AT-SPI; reusable once bootstrap and input transport are adapted to the Tuna Desktop VM. |
| `adapt` | Needs a Tuna Desktop/Marlin-specific replacement for a GNOME or Fedora assumption. |
| `insufficient-alone` | Panel navigation or a gsettings round trip; not evidence of behaviour. |
| `unsupported` | Depends on a distribution mechanism Marlin lacks (for example `rpm -q`). |
| `inapplicable` | Superseded by a stronger existing Tuna Desktop test. |

The manifest's `policy` block must keep whole-scenario retry, CLI-only
fallback, starting GNOME Shell, a fake `Shell.Eval` provider and "counts as
desktop compatibility" all `false`. Check it with
`python3 scripts/tests/test_bluefin_atspi_scope.py`.

## Not done yet

- Running the selected scenarios on the installed Tuna Desktop session of a qualified
  Marlin candidate, with process-identity and AT-SPI owner receipts.
- The Tuna Desktop VM bootstrap and physical-input transport.
- A CI lane and release gate for exact candidate heads.
