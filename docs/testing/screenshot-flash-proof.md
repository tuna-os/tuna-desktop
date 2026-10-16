# Screenshot flash runtime proof

Run `scripts/tuna-screenshot-flash-proof --bundle <frozen-bundle> --artifacts <new-directory>` with system Python and Pillow, Xvfb, scrot, xdotool, and D-Bus installed. The bundle must include a clean source revision and SHA-256 hashes in `manifest.json`. The runner checks the frozen binaries and the actual compositor process bytes.

The nested compositor supervises a fixture that calls the production screenshot service. A private display and bus isolate each run. This exercises real rendered pixels and saved PNGs; it does not exercise the GTK screenshot picker or physical display hardware.

Assertions cover:

- An unrelated caller cannot flash the screen.
- A capture without feedback leaves the output unchanged.
- A successful capture starts white feedback, reaches quarter opacity at 250 ms, and restores the background at 500 ms.
- The saved PNG excludes feedback and the screenshot reply waits for feedback completion.
- A failed capture produces no feedback.
- FlashArea affects its bounded rectangle, leaves outside pixels unchanged, and rejects invalid areas.
- Process identity stays unchanged and owned processes, activated helpers, and the private runtime are cleaned up.

The animation clock freezes at its actual initialization offset. Samples advance relative to that offset. Each polling capture uses a fresh filename: scrot preserves an existing destination by creating a numbered file, so rereading the original destination observes stale pixels.

## Qualification recorded on 2026-10-10

Two consecutive complete runs passed against frozen source `0a7b68a6b4c0168f536b2a9eb46c61373d1a8dc2`, compositor SHA-256 `d6fd9648e9cab920fb0855dc56364d6de4d9e81eab3eddc754d3e51860e948a0`, and proof script SHA-256 `c6983f9b22072ba4735220baee13ecc1a2b8c8d723d97d6771119b02348e92ca`.

Local receipts and PNGs are in `/tmp/tuna-v1-flash-combined-0a7-proof-6` and `/tmp/tuna-v1-flash-combined-0a7-proof-7`. Both observed background RGB `[20, 23, 28]`, start `[255, 255, 255]`, midpoint `[79, 81, 85]`, and the original background at completion. Both receipts record successful owned cleanup and process identity verification after all cases.

Earlier failed runs remain available. In attempt 4, `03-start.png` retained the original background while later scrot-generated files contained white pixels. That failure exposed a stale capture read in the proof; it did not establish a renderer defect. The corrected runner retains every polling image under an explicit unique name.

These checks establish the listed nested runtime behaviors. GNOME performance comparisons, physical GPU output, GTK picker integration, and the full v1 release gates require their own evidence.
