---
created_date: "2026-09-30"
document_status: draft
---

All success metrics are covered by automated behavioural tests; no manual test plan is required.

- First-frame paint → `panel::tests::live::overview_open_paints_window_pixels_to_committed_buffer` and `panel::tests::live::panel_only_iteration_paints_open_overview` assert non-backdrop pixels on the committed buffer.
- Zero blank reports (open / resize / empty) → `panel::tests::live::overview_repaints_after_output_resize` (buffer matches the new size) and `panel::tests::live::empty_overview_commits_backdrop` (full-size buffer with no windows).
- Typing responsiveness → `panel::tests::live::typing_during_paint_arrives_complete` drives five keystrokes through the real key path during paint and asserts all five land in order.
