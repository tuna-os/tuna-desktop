---
created_date: "2026-09-30"
document_status: draft
---

All success metrics are covered by automated tests; no manual test plan is required.

- Sample extension cell renders and reacts → `extensions::tests::sample_cell_contributes_bar_cell`, `extensions::tests::press_reaches_declaring_script`, and `panel::tests::live::script_cell_paints_in_strip`.
- Hostile script battery stays contained → `extensions::tests::filesystem_access_denied_and_disabled`, `extensions::tests::network_and_spawn_denied`, `extensions::tests::import_from_disk_refused`, `extensions::tests::infinite_loop_budgeted_and_disabled`, `extensions::tests::syntax_error_disables_with_note`.
- Editing a script updates without restart → `panel::tests::live::edited_script_reloads_and_removed_drops`.
