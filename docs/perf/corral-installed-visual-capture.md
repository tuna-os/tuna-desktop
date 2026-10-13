# Installed fixture visual capture

`scripts/tuna-corral-visual-capture` captures desktop, overview, quick settings, calendar, app grid and Escape return
on an already imported, active Tuna GTK Wayland session. It does not create VMs,
install guest packages, restart sessions, or assert completion of the 72-row
capability matrix. The paired benchmark image remains a CI fixture, with a test
user, autologin and additional instrumentation. Captures do not qualify shipping
defaults, typed greeter login, hardware, GPU, power or transition parity.

This observer is reconciled onto the existing export/SSH/ESP source. Its tooling CI is source-only and exercises no guest. The original authenticated export and successful import receipts are mandatory.
Supply the exact candidate source and original package CI run as intent; these
are checked before opening transport. The separately reviewed Corral importer (PR #599; not supplied by this observer leaf) requires
`--candidate-sha` and `--package-run`, and checks them before creating any object.
Its `--source-sha` continues to identify the exporter observer revision.

```sh
python3 scripts/tuna-corral-visual-capture \
  --context admin@aws-migration --namespace corral-vms --vm OWNED_VM \
  --key OWNED_PRIVATE_KEY --public-key EXACT_EXPORTED_PUBLIC_KEY_FILE \
  --known-hosts OWNED_KNOWN_HOSTS \
  --export-dir ORIGINAL_EXPORT_DIR --import-receipt ORIGINAL_IMPORT_RECEIPT \
  --reference-image ORIGINAL_REFERENCE_JSON --tuna-payload ORIGINAL_PAYLOAD_JSON \
  --tuna-selection ORIGINAL_SELECTION_JSON \
  --candidate-sha EXACT_40_CHARACTER_CANDIDATE --package-run ORIGINAL_PACKAGE_RUN \
  --out NEW_CAPTURE_DIRECTORY
```

The guest uses existing `python-gobject` with GI `Atspi` 2.0 and the session
accessibility bus. These come from the GNOME baseline and exporter; the runner
requires no separate `pyatspi` package. A missing introspection namespace is
reported as unobserved; the runner does not silently install it.
Each observation verifies native binary hashes, the unique GTK process, its start
time, the current boot, one active Wayland login and application PID on that user's
AT-SPI bus. ScreenSaver.GetActive must report unlocked. It selects controls by
their source-defined accessible names: Activities, System, Search, Dark Style and
Volume, Date and Time, Previous month, Next month, Show Apps and App Grid. This scope assumes the English interface used by CI. Other locales
can fail observation and are not inferred from pixels.

Admission also calls the existing selected-desktop check, rejects a same-user
GNOME Shell, and ties GTK to the unique Tuna compositor by process ancestry and
the exact same logind session cgroup. Both running executable images are hashed
through `/proc/PID/exe`. Native DRM evidence requires a live primary DRM character
device descriptor and the source-defined DRM output journal event from that exact
PID, UID, boot and process lifetime. Explicit nested backend arguments, missing
device/log evidence and changed native identity across a frame are refused.
This conservative rule can leave an unusual valid session unobserved; it never
promotes a nested Tuna window inside GNOME to native-session evidence.

Escape resets the popover/overview, Super opens overview, and the showing System
control's actual AT-SPI action opens quick settings. Calendar uses the live Date and Time action and requires both month navigation controls. App Grid uses the live Show Apps action and requires a showing descendant launcher whose label matches the ordinary user’s actual Gio installed application inventory. No app or calendar fixture is seeded. Folder/page variants and calendar event delivery remain outside this subset.

State acquisition polls fresh trees until two consecutive consumer-control observations match, with an absolute host deadline and a guest-owned timeout. Process, boot and session changes fail immediately. State observations bracket
each framebuffer request. Missing controls, changed boot/process/session identity,
uniform pixels, and duplicate images between different states refuse acceptance. The explicit desktop-return state may equal the initial desktop pixels, but only after the preceding app-grid state was accepted and real Escape input delivered. It proves return of shell controls, not restoration of a previously focused app.
Framebuffer dimensions come from the real RFB response, without a 1280×800 guard.

Every accepted frame records its SHA256, observation SHA256, dimensions, boot and
VM identities, candidate source, package hash and immutable fixture image ID.
The manifest includes observer revision, original artifact/import receipt hash,
reference identity and runner/helper hashes. Original receipts are read-only.
An observation is not a visual rendering review: each still says
`visual_review: required` and `capability_qualified: false`.

On a partial attempt, each unsuccessful state is explicitly unobserved and the
command exits nonzero. If final ownership verification fails, captured states are
marked stale. Use a new output directory for every attempt; never combine attempts
or attach release/current labels to these frames.

## Real greeter overlay remains separate

This runner offers no automatic greeter restoration. The performance export
changes GDM defaults and the session Exec line. An eventual explicit native-login
overlay must restore the original package session entry with the documented
preview Name rewrite and verify both package/preview hashes; disable autologin,
the performance sampling service and CI session reseeding; record every changed
file/mount/service and the VM/boot identity. Read-only composefs requires a verified
owned overlay rather than an assumed writable `/usr`. Even then the result is
fixture-derived native-login evidence until a shipping image is separately tested.

Run paired performance before applying any such overlay, or use separate owned
storage. The paired selector deliberately rewrites login configuration and reboots.
