# Screenshot flash feedback

The compositor consumes the `flash` argument of `Screenshot` and `ScreenshotWindow`, after the PNG has been saved successfully. `FlashArea` uses the same authenticated caller policy, rechecks that the session is unlocked on delivery, and rejects invalid output bounds using widened arithmetic.

The reference is [GNOME Shell 51 Flashspot](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/screenshot.js#L3225): white immediately, fading over 500 ms with out-quad. The shared motion preference scales that duration, retains the fade in reduced motion, and suppresses it with animations off. Locking clears feedback. At most 32 simultaneous feedback rectangles are retained.

Feedback is a separate output scene pass, above normal content and below the idle shield. Capture and screencast offscreen scenes leave this pass empty, so the feedback cannot enter the saved PNG or recording. Native damage signatures include the pass and its opacity commit metadata; removal also changes the scene signature. Rectangles are projected independently for each output.

## Verification scope

Three independent state tests passed: 1.0/0.25/absent at 0/250/500 ms, zero-duration and invalid geometry rejection, and bounded concurrent feedback with immediate clearing. This is not a runtime pixel receipt. The source still requires the guarded compositor build, actual authenticated D-Bus requests, output pixel measurements, false/failed/locked negative cases, and paired GNOME/native hardware qualification before this part of P-AN-10 can pass.
