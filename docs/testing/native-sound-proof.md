# Native sound consumers and qualification

The native media-key and output-slider consumers read the live GNOME sound
preferences. Disabling overamplification queues a backend clamp, including
when volume was amplified before the shell started. Backend writes are
serialized; the slider updates mute state and reads the backend back before
feedback. Quiet keys, unchanged/muted output, disabled event sounds, and an
unknown default sink suppress feedback. A Running default sink suppresses
feedback unless the shell's current feedback player actually has a Running
output stream actively linked to that sink.

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
D-Bus daemons, Xvfb, wpctl/pactl, and a C compiler with pkg-config,
PipeWire and Wayland client/scanner/protocol development files. The
`--audio-fixture pw-tools` diagnostic additionally uses pw-record/pw-play.
It disables hardware
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

## Diagnostics from the combined v1 bundle

The clean `0a7b68a6b4c0168f536b2a9eb46c61373d1a8dc2` bundle has passing
nine-case delivered receipts and retained intermittent positive failures.
It is not yet qualified as repeatable. In attempts 8 and 9, the actual volume
changed to 0.46, the sink was suspended immediately before input, and the
real player was invoked, but the passive recorder captured silence.
Attempt 9 observed the full five-second player deadline plus one second;
extending the observation window alone did not resolve the failure.

`--diagnose-player` records the resolved player and exec-wrapper hashes,
invocation time and arguments, and real stderr. The wrapper replaces itself
with the real player, retaining the production parent-death signal and alarm.
The receipt also retains the pre-input sink state and observation duration.

A separate diagnostic run in
`/tmp/tuna-v1-sound-combined-0a7-lifecycle-2` passed all nine cases. An additional
library probe recorded successful libcanberra callbacks and successful player
main returns for volume and system-bell events. That probe modifies the
diagnostic process environment and provides lifecycle evidence; it is not a
performance measurement or proof that previous silent attempts succeeded.

The passive recorder defaults to 100 ms latency. An explicit 256-sample
experiment captured cold volume events, but a subsequent playing case failed
the unchanged sine-continuity assertion. Offline analysis of that capture
found a phase discontinuity exactly matching 256 missing samples at 48 kHz.
The analysis is retained in `/tmp/tuna-v1-playing-phase-analysis.json`; it
does not turn that failed run into a pass.

A scratch diagnostic using 1024 samples passed all nine cases on both the
original bundle (`/tmp/tuna-v1-sound-buffer1024-1`) and the new feedback bundle
(`/tmp/tuna-v1-sound-feedback-b02-buffer1024-1`). The new run's playing
residual was 3.573564211029281, below the unchanged limit of 5, and cleanup
passed. This is limited diagnostic evidence, not established repeatability
or a recorder latency measurement.

## Owned feedback build

Clean source `b02bc9f0c2c23c81731831e7c3704f4c68a1d510` completed a fresh
workspace-package build under the shared guard. Schema tests passed 52 unit
and 29 integration tests; compositor tests passed 351 with one ignored;
host tests passed 327; GTK tests passed 126 with two ignored. Strict
all-target Clippy and the union build of three desktop binaries and three
native examples passed. All seven frozen executable hashes were checked
against `/tmp/tuna-v1-feedback-replacement-binaries-b02bc9f0/manifest.json`.

The new bundle passed the nine-case delivered diagnostic above. A separate
replacement check sends a second real volume key only after observing the
first owned player's Running stream and active sink link, then requires a
different feedback PID. Its first attempt failed in the fixture's parser:
`pw-dump` returned consecutive event arrays during stream creation, and
`json.loads` rejected the extra array. The failed receipt and successful
cleanup are retained at `/tmp/tuna-v1-sound-feedback-b02-replacement-1`.
The corrected scratch parser folds complete arrays and still rejects
malformed JSON; the replacement behavior remains unqualified until that
runtime check succeeds.

The second replacement attempt lost the shipped 67 ms player before the
fixture's process inspection. The third used an explicitly selected custom
two-second sound theme so the original player could be inspected while its
stream was Running. Its replacement case observed a new player and volume
0.52, but the complete ten-case run failed later on playing continuity:
residual 8.641056208623368 exceeded 5 even at 1024 samples. The receipt at
`/tmp/tuna-v1-sound-feedback-b02-replacement-3` retains that failure and
successful cleanup. Consequently neither recorder buffer size is qualified
as repeatable, and this run is not an aggregate audio pass. A separate
three-second-theme overlap diagnostic checks the old bundle to distinguish
the policy change from natural playback completion; its results follow.

That matched comparison completed: the updated bundle passed the two-case
overlap diagnostic at `/tmp/tuna-v1-sound-feedback-b02-overlap-1`; the old
bundle failed exactly when waiting for the second feedback PID at
`/tmp/tuna-v1-sound-feedback-old-overlap-control-1`. Both observed the first
real player's Running output stream and active link before the second key,
and both verified owned cleanup. The old receipt remains a failed run; it
is a negative control for this policy change. This comparison does not
qualify the full nine-case suite.

The reusable proof now exposes `--owned-overlap-only`, requiring
`--delivered --diagnose-player`, and reports its two-case scope explicitly.
It uses a selected custom three-second sound theme because the shipped
67 ms sound ends too quickly for the graph inspection. The new tool itself
passed against the updated bundle at
`/tmp/tuna-v1-sound-feedback-b02-owned-tool-1`. The default mode still requires
all nine original cases and their original assertions.
`--recorder-latency-samples` allows an explicit bounded capture setting;
the pw-tools path has not established repeatability.

The failed 1024-sample playing capture also has an offline phase analysis at
`/tmp/tuna-v1-playing-buffer1024-analysis.json`: neighboring stable blocks
have equal amplitude and a phase jump matching one 1024-sample quantum.
A separate memory-recorder experiment removed disk I/O from its PipeWire
process callback and wrote the WAV only after disconnecting. The run at
`/tmp/tuna-v1-sound-feedback-b02-memory-capture-3` still failed the unchanged
continuity gate (residual 8.042682078299364). Its own counters reported zero
invalid and overflow buffers, and owned cleanup passed. This rejects the
hypothesis that recorder file I/O alone explains the discontinuity; it does
not establish whether the remaining loss is upstream playback, the graph,
or capture scheduling. The experimental recorder remains outside the repo.

Two subsequent full nine-case diagnostics used both a memory-fed tone player
and memory recorder, retaining real PipeWire streams and the same generated
tone bytes. Both completed with unchanged peak, sine-continuity, backend,
source-identity, and cleanup assertions:
`/tmp/tuna-v1-sound-feedback-b02-memory-both-1` and
`/tmp/tuna-v1-sound-feedback-b02-memory-both-2`. Playing residuals were
3.5817598326003 and 3.571930605143872. Recorder invalid/overflow/error counters
and player invalid/error counters were zero. These results support the
revised fixture on this host; they do not identify a single cause for the
old fixture's discontinuities or qualify physical audio or performance.

The reusable proof now defaults to the source-owned memory helpers and
1024 samples. It stages their source, compiles with strict C warnings,
records source/binary hashes and the PipeWire development version, checks
the running executable bytes, and rejects helper errors, overflow, or
frame-count mismatches. File I/O happens outside the process callbacks.
The original diagnostic path remains available explicitly as
`--audio-fixture pw-tools`; there is no automatic fallback. The integrated
default completed its own nine-case run at
`/tmp/tuna-v1-sound-feedback-b02-memory-tool-1`, with verified running helper
identities and owned cleanup. The unchanged integrated tool also passed all
nine cases at `/tmp/tuna-v1-sound-feedback-b02-memory-tool-2`. Playing sine
residuals were 4.966181511597964 and 3.5829795994198195, respectively, below
the unchanged limit of 5. Both runs verified the delivered b02bc9f0 native
executables, recorded the source and running bytes of the memory helpers,
reported zero helper invalid-buffer, overflow, and error counters, and
completed owned cleanup. These repeat results are independent of the earlier
scratch diagnostics and qualify only this private null-sink fixture.

## Open qualification and behavior gaps

The private fixture does not qualify physical audio devices, hardware keys,
visual bell pixels, Xwayland bells, notification/input-click sounds, or full
GNOME Settings mixer/profile and alert-selection paths. Feedback currently
has a candidate exemption for its owned feedback player: the real player
must have a Running output stream linked actively to the current default
sink. Unknown state and unrelated playback remain suppressive. This follows
[GNOME 51's media-key feedback policy](https://github.com/GNOME/gnome-settings-daemon/blob/51.0/plugins/media-keys/gsd-media-keys-manager.c),
which checks its own event playback before treating sink Running as external
playback. The fresh build and first nine-case delivered proof passed;
the separate two-case owned-overlap proof also passed with an observed Running
owned stream and a replacement player PID. Its old-bundle negative control
failed to start that replacement. Repeated nine-case native key proofs passed
as described above; physical-device and full sound-policy proofs remain open.
Slider feedback uses the shared debounce rather than GNOME's drag-end
specific behavior. These gaps prevent a full sound parity claim. No audio
receipt establishes a performance win over GNOME.
