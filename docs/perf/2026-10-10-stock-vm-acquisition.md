# Stock GNOME 51 VM acquisition: 2026-10-10

Evidence: [performance run 38058410812](https://github.com/tuna-os/tuna-desktop/actions/runs/38058410812), retained artifact `11675220019` (`marlin-performance`). The workflow failed because the third instrumented GNOME capture had probe misses. The separate stock profile completed all six acquisition commands.

The selected Tuna package was built from main commit `c387326d067a53b7b583aafb9e0206fba0fab59c`, not the current implementation branch. Stock reference identities record `mutter 51.0-1` and `gnome-shell 1:51.0-1`. These VM observations do not qualify current v1, native input-to-frame latency, physical GPU use, or total power.

## Stock observations

| Alternating pair | Desktop | Overview host-observed upper-bound p50 (ms) | Idle test-UID PSS p50 (KiB) | Sampled idle CPU p50 (%) |
|---|---|---:|---:|---:|
| 1 | gnome | 160.500 | 529379 | 0.000 |
| 1 | tuna | 162.558 | 445544 | 1.000 |
| 2 | gnome | 167.194 | 527211 | 0.000 |
| 2 | tuna | 160.779 | 405593 | 1.000 |
| 3 | gnome | 159.924 | 527124 | 0.000 |
| 3 | tuna | 161.459 | 398290 | 1.000 |

QMP observations are host-visible bounds. CPU sampling excludes exited processes and can miss their work. PSS covers the test UID, excluding the observer and its descendants; root/GDM services are outside that scope. No hardware or release performance victory follows from this table. Tuna has a sampled idle CPU gap to address even though its test-UID PSS is lower.

## Instrumented rejection

The third diagnostic GNOME acquisition reported misses for timeout and timer entry/return probes. Its ring overrun counters were zero, the watchdog did not expire, and cleanup had no errors. The acquisition correctly remained rejected; zero ring overruns alone do not establish complete tracing.

## Retained receipt hashes

The downloaded artifact is retained locally at `/tmp/tuna-v1-performance-run-38058410812`. The following SHA256 values identify its small receipt files for independent checking:

| Artifact path | SHA256 |
|---|---|
| `package-selection.json` | `0e1fb3a2197a604538cf0944ec0c4c5b12cea387a6b3ce8b120c2781e0f3d669` |
| `stock/acquisition-status.json` | `a199870d5e9faafa575c279565ebe063519e3750cf739226bbee5a2ff03c8135` |
| `repeat-3/gnome/kernel-vblank-source.json` | `f174765056e4cce14e7a0451b808535faadfbba495c7971c455065e11f098bba` |
