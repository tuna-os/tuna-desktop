# Privacy device controls on the published Marlin image (#356, P-ST-04)

Run 2026-10-30, Corral KubeVirt VM `tuna-q-privacy` (4 vCPU, 6 GiB,
bochs-drm `Virtual-1` 1280x800, chassis `vm`, no sound card, no camera,
no USB passthrough), deleted afterwards (`corral delete --wait`; `corral
list` no longer shows it).

- Image: `ghcr.io/tuna-os/marlin@sha256:ae6363c185015849701328fc38694f5ddcc0b4c804034a1d8d05577f1c70cf7d`
  (`marlin:tuna`, version `cbb3569`, created 2026-10-29T21:46:44Z);
  booted manifest `sha256:a182a20b732b6f8954be4afd7c0b47fa67306143100b5377d0b015a873415beb`.
- Shipped packages: `tuna-desktop 1.0.0beta.1-1` (built 2026-10-15, from
  the `v1.0.0-beta.1` tag), gnome-settings-daemon 51.1, gnome-control-center
  51.1, xdg-desktop-portal 1.22.1, xdg-desktop-portal-gnome 51.0,
  pipewire 1.6.10, wireplumber 0.5.18, fwupd 2.1.9, geoclue 2.8.2,
  bolt 0.9.11. `pacman -Q usbguard`: `error: package 'usbguard' was not found`;
  no `/usr/lib/systemd/system/usbguard*`, no `/usr/bin/usbguard*`.
- Input: GDM login, Settings navigation and every Settings switch/button
  below were driven with `corral pointer`/`corral type`/`corral key` (real
  VNC input). GNOME 51 Settings has no microphone or sound-output switch
  (gnome-control-center 51.0 `panels/privacy/` has only `camera/`;
  `cc-applications-panel.c` only reads the keys), so those two keys were
  written with `gsettings set`.
- Probe: [`probe.sh`](probe.sh), run in the user session: the three keys,
  each Lockdown backend's properties, a real `org.freedesktop.portal.Camera`
  client (`AccessCamera`, `OpenPipeWireRemote`), and the PipeWire state of
  the `tuna-q-*` virtual nodes (`pw-dump`, `wpctl get-volume`).

## A. Shipped image as published (nothing sideloaded)

Shipped `tuna-shell-gtk` predates #654: it contains no `disable-camera`
string and owns no `org.freedesktop.impl.portal.desktop.tuna`; the shipped
`tuna-portals.conf` has no `Lockdown=tuna` line and no `tuna.portal` is
installed, so Lockdown resolves to xdg-desktop-portal-gnome. The shipped
`tuna-session.target` wants no gnome-settings-daemon plugin: no `gsd-*`
process runs (no Housekeeping, no UsbProtection on the session bus).

| Step | Observed |
|---|---|
| Baseline | keys false; `impl.gnome` Lockdown false; `AccessCamera` returns a request handle |
| Cameras > Camera Access switched off by click ([06](06-camera-off.png)) | `disable-camera=true`; `impl.gnome` `disable-camera` true; `AccessCamera` and `OpenPipeWireRemote`: `org.freedesktop.portal.Error.NotAllowed: Camera access disabled` |
| Switched back on by click ([07](07-camera-on.png)) | `AccessCamera` returns a request handle again ([shipped-probe-2.txt](shipped-probe-2.txt)) |
| `disable-microphone`/`disable-sound-output` true | `impl.gnome` reports true; virtual `Audio/Sink`/`Audio/Source` stay unmuted (GNOME-equal: GNOME 51 has no enforcer) |
| Screen Lock page ([08](08-screen-lock.png)) | no Privacy Screen row (GetCurrentState has no `privacy-screen-state`: bochs-drm has no KMS privacy screen); no USB Protection row (`org.gnome.SettingsDaemon.UsbProtection`: `ServiceUnknown`) |
| File History & Trash > Empty Trash… > Empty Trash by click ([12](12-empty-trash-dialog.png), [13](13-empty-trash-shipped-after.png)) | `~/.local/share/Trash/files/tq-trash-shipped.txt` is still there: the button calls gsd Housekeeping `EmptyTrash`, which is not running |
| Privacy panel ([04](04-privacy-panel.png)) | no Diagnostics row (no `org.freedesktop.problems.daemon`; GNOME 51 `cc-diagnostics-page.c` hides it), no Device Security row (chassis `vm`; `cc-firmware-security-page.c` `update_page_visibility` hides it), no Thunderbolt row; Location page loads, geoclue is D-Bus activated (static, inactive until used); `fwupd.service` active |

## B. Candidate main sideloaded on the published image

`bootc usroverlay`, then `pacman -U --nodeps` of main's CI package
(`tuna-desktop-1.0.0beta.2-1-x86_64.pkg.tar.zst` from ci run
[41051572900](https://github.com/tuna-os/tuna-desktop/actions/runs/41051572900),
main `a3308583`, sha256 `c1d3b495ac8e075744b0931869b9aeb164b8a5e32825e51e3ca5d709469600e5`),
logout and login through GDM. Running executables:
`tuna-compositor` sha256 `9e63b261fb67a6e55eac9ec3835ea171cf0ef1738e40db9fc67fc175b5773a80`,
`tuna-shell-gtk` sha256 `3f28fe1fb4c15218ccc87494083357cea7e86b848894ba0104b57094541b1e76`.
`tuna-shell-gtk` owns `org.freedesktop.impl.portal.desktop.tuna`; all
gsd plugins run, including `gsd-housekeeping` and `gsd-usb-protection`.

| Step | Observed |
|---|---|
| Baseline ([candidate-01](candidate-01-baseline.txt)) | `impl.tuna` Lockdown false; `AccessCamera` reaches the real "Allow Apps to Use the Camera?" prompt ([19](19-candidate-allowed-reaches-access-prompt.png)), answered Deny |
| Camera Access switched off by click ([21](21-candidate-camera-off.png), [candidate-02](candidate-02-camera-off.txt)) | `impl.tuna` `disable-camera` true; portal `Camera access disabled`; shell logs `privacy enforcement [DestroyNode { node: 60 }]` for the simulated camera (`videotestsrc` adapter node with `device.api=v4l2`), but the node stayed; a manual `pw-cli destroy 60` of the same node broke the PipeWire connection and the graph restarted, so this fixture cannot qualify the destroy path |
| Microphone and sound output disabled, then restored ([candidate-03](candidate-03-mic-output.txt)) | shell logs `SetMute { node: 53, muted: true }` then `muted: false`; the `tuna-q-*` nodes were gone after the PipeWire restart, so no `wpctl` readback was retained |
| All restored | `AccessCamera` returns a request handle again |

Not exercised in this run: restart persistence across logout/login with
the keys set, USBGuard at lock (USBGuard absent), automatic trash/temp
purge ages, a real camera, a hardware privacy screen.
