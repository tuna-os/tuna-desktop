---
created_date: "2026-10-09"
document_status: draft
---

# Tuna Desktop v1 implementation changelog (in progress)

## v1 victory signal (task 1)

The v1 program now publishes a nightly machine-readable victory
signal instead of relying on hand-edited status: a script reads the
parity ledger pass fraction and the open gap-issue count, marks the
perf and image verdicts pending on their lanes, and uploads the
result as a CI artifact with a step summary.

Why it matters: the v1 definition promises a checklist that reports
live instrument state. This is the first instrument wired to that
promise; every later task feeds rows into the same signal.

Deviations from the plan: None.

## 2026-10-17 — CI iteration support (rust-wayland-desktop)

Merged PR721 as73ebfed07f341c5ef1a7ef50727ecb6d05533fe7: independent lightweight lane selection, earlier admissible proof starts, trusted-main cache writers, native-environment keys, missing standalone/proof target coverage, and persistent separate grid/fingerprint build targets. PR and trusted-main runs passed all19 jobs without retry; all source/executable admission, runtime proofs and final gates remain in force. Existing testing documentation records the contract and why cache/snapshot contents never substitute for fresh proof.


Follow-through PR724 passed all40 exact-head checks at34dbe834 and was normally squash-merged as58ed90b502c6019c9cec4965124d27d323342f91. It adds auxiliary trusted-main writers/native identities and the demonstrated native-auth fixture PID-publication fix. Deterministic before/after smoke plus40 complete native proofs passed; original early compositor exit is not retrospectively attributed to that race. External #718/#606 changes were preserved. Qualification: https://github.com/tuna-os/tuna-desktop/pull/724#issuecomment-6230847300 .

Merged-main measured coverage passed and seeded720,515,080 bytes. Primary main restored nine caches, but its GTK IME lifecycle gate failed and Arch still saved only983 bytes; do not report that run as green or Arch cache coverage as working. Retained IME observer was waiting for its initial indicator before any lock phase. No retry, deadline extension or product fix was applied.

PR734 atcfc8891d285c0f893539271e1cd6790eaa1d0d6b passed all19 exact-head CI jobs/all16 real lanes without retry and was normally guarded squash-merged as2617bf54e3efb083122197d9c657a1452ebe551b. It fixes the demonstrated cleanup boundary: Cargo home must be outside the workspace or the pinned action excludes registry dependencies as local crates. Real build/pinned cleanup/archive/restore/rebuild proved dependency reuse after the correction. Native main package job117350600224 then passed and actually uploaded516,497,262 bytes; GitHub inventory confirms archive9084951788 on refs/heads/main. Arch retention is now substantive, not the prior983-byte empty archive. Normal full main CI39016825550 then passed all19 jobs/all16 real lanes in40m31s, without retry, with nine real restores and all ten trusted-main writer guards true. Existing testing docs explain the invariant; supporting CI verification is complete.

Observed warm jobs are faster for several lanes, but hit retention and runner delays vary; no controlled end-to-end speedup is claimed. Active plan20261009134949-7190f8bf-0001-tuna-desktop-v1-definition stays implement, all nine v1 acceptance tasks unchecked. No ORAS infrastructure, paid quota change, blind retry or proof waiver.
