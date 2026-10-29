---
tags: [drm, kms, suspend, presentation-time]
---

# Valid KMS completion can precede a userspace submission marker

A native suspend/wake VM diagnostic recorded a newly submitted modeset completion at monotonic 203.042551 s, before the userspace cutoff at 203.042624625 s, about 74 microseconds earlier. Rejecting that valid completion as stale left the output pending forever and the wake display blank.

Do not identify stale page flips by comparing their kernel vblank timestamp with a userspace submission marker. Track ownership of the queued frame and establish an explicit reset boundary: after a successful blocking device reset, discard invalidated pending feedback and drain old events before submitting a new frame. A failed reset must not reopen scanout admission. Preserve genuine kernel presentation timestamps; never manufacture completion to recover the pipeline.

Diagnostic: PR #265, commit ee77ae332bace5a745d778e4cedacbf90f090a69, native VM run 37203685773. Corrective candidate caa0fe8376045d83d647e1438667a3f61225491c still requires its complete runtime qualification; the timestamp finding itself is directly measured.
