// Tuna Desktop sample extension: a greeting bar cell.
//
// Install: copy this file into the extension directory
// (`~/.local/share/tuna-shell/extensions/hello.rhai` by default).
// The legacy shell (`tuna-shell-host`) picks it up without a restart;
// the GTK shell loads no scripts.
//
// API used here:
//   bar_cell(text, icon)  — contribute the strip cell.
//   on_press(id)          — declare the press action id.
//   press(id)             — called by the shell on press (optional).
//   notice(text)          — record a user-visible note (optional).

bar_cell("hello", "face-smile");
on_press("hello-wave");

fn press(id) {
    notice("hello cell pressed: " + id);
}
