# Native sound consumers and qualification

The native media-key and output-slider consumers read the live GNOME sound
preferences. Disabling overamplification queues a backend clamp, including
when volume was amplified before the shell started. Backend writes are
serialized; the slider updates mute state and reads the backend back before
feedback. Quiet keys, unchanged/muted output, disabled event sounds, and an
unknown or Running default sink suppress feedback.

The compositor consumes live audible/visual bell policy through the shell
control protocol. Bell audio stays silent before the first trusted policy.
The native xdg-system-bell handler associates a frame flash with the supplied
currently rendered toplevel; a null target flashes the fullscreen view.
The visual overlay uses the Mutter 51 black opacity, quadratic 50 ms legs,
500 ms rate limit, and repeat threshold. Visual presentation still requires
its own pixel proof; audio receipts do not qualify it.

Both the player and wpctl children have a Linux parent-death SIGKILL and a
five-second kernel alarm independent of the GTK/compositor main loop.
Main-loop cleanup also bounds/reaps owned children. Player errors are silent.

## Runtime dependencies

Arch: `libcanberra` supplies `canberra-gtk-play` and the Pulse backend;
`sound-theme-freedesktop` supplies `audio-volume-change.oga` and `bell.oga`.
`bell-window-system` resolves through libcanberra's fallback to `bell.oga`;
there is no requirement for a literal `bell-window-system.oga` asset.
Ubuntu proof host: `gnome-session-canberra`, `libcanberra-pulse`, and
`sound-theme-freedesktop`. The GTK library package alone does not supply the
player. Missing players/assets fail silently and cannot qualify an audible
positive case.

## Captured private audio proof

Build and freeze a stable source revision under the shared build lock. The
identity JSON records `source_revision` and SHA-256 for all three desktop
binaries and the two source-owned audio examples. Then run:

```sh
python3 scripts/lib/tuna-sound-consumers-proof.py \
  --volume-client /path/to/frozen/tuna-volume-feedback-client \
  --bell-client /path/to/frozen/tuna-bell-audio-client \
  --identity /path/to/frozen/identity.json \
  --source-revision EXACT_FROZEN_REVISION \
  --artifacts /path/to/new-artifacts --delivered
```

The fixture currently requires WirePlumber 0.4, private PipeWire/Pulse and
D-Bus daemons, Xvfb, wpctl/pactl/pw-record/pw-play, and a C compiler with
Wayland client/scanner/protocol development files. It disables hardware
monitors, rejects discovered ALSA/BlueZ devices, and uses a null sink with a
passive recording stream. Xvfb must use `-noreset`: resetting after the last
client disconnects otherwise rewrites its closed display-number pipe and
terminates the display. The recorder's graph link and backend state are
observed before input. No physical audio output is used.

`--delivered` sends genuine focused X11 host media keys to the nested desktop
and real Wayland xdg-system-bell requests. The receipt checks the original
compositor and org.gnome.Shell owner's `/proc/PID/exe` bytes each case.
It includes source/binary/player/asset/protocol hashes, input commands,
backend volume, captured WAV frame count/peak/RMS, and bounded cleanup of
process groups and bus-activated helpers using pidfds. The playing case
feeds an actual low-amplitude tone: sink Running must suppress extra event
sound while volume changes. Silent cases may produce zero frames because
the passive sink remains asleep; the audible positives qualify the same
capture path. Failure of any positive or identity check rejects the run.

Without `--delivered`, the examples execute production source consumer
modules directly. That scope does not qualify protocol or key delivery.

## Open qualification and behavior gaps

The private fixture does not qualify physical audio devices, hardware keys,
visual bell pixels, Xwayland bells, notification/input-click sounds, or full
GNOME Settings mixer/profile and alert-selection paths. Feedback currently
suppresses when any default-sink playback is Running, including this shell's
own current player; GNOME's media-key consumer exempts its own feedback.
Slider feedback uses the shared debounce rather than GNOME's drag-end
specific behavior. These gaps prevent a full sound parity claim. No audio
receipt establishes a performance win over GNOME.
