//! GNOME 51's Run a Command dialog (`js/ui/runDialog.js`), opened by
//! `panel-run-dialog` (Alt+F2) in NORMAL and OVERVIEW modes (main.js).
//!
//! As in GNOME: a modal dialog titled "Run a Command" with one entry
//! and the "Press ESC to close" caption; Return runs the line through
//! GLib's shell parser on `PATH` (Ctrl+Return inside
//! `org.gnome.desktop.default-applications.terminal`'s terminal); a
//! line that is not a command but names an existing file (absolute, or
//! relative to the home directory, `~` allowed) opens in its default
//! application; failures stay in the dialog as its caption; Tab
//! completes commands from `PATH` and the home directory, or paths
//! once the word holds a `/`; Up and Down walk
//! `org.gnome.shell command-history` (GNOME's `HistoryManager`, 512
//! entries); Escape closes.
//!
//! Administrative lockdown (#357): `open` refuses while
//! `org.gnome.desktop.lockdown disable-command-line` denies, re-read on
//! every open exactly as `RunDialog.open()` does.
//!
//! Not carried over: GNOME Shell's developer commands (`lg`, `rt`,
//! `debugexit`, `check_cloexec_fds`), which drive Looking Glass and
//! the St theme of GNOME Shell's own process; here those words run as
//! ordinary command lines.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// GNOME Shell's schema and key for the dialog's history.
const SHELL_SCHEMA: &str = "org.gnome.shell";
const HISTORY_KEY: &str = "command-history";
/// GNOME's terminal for Ctrl+Return.
const TERMINAL_SCHEMA: &str = "org.gnome.desktop.default-applications.terminal";
/// `HistoryManager`'s `DEFAULT_LIMIT`.
pub const HISTORY_LIMIT: usize = 512;
/// The dialog's resting caption.
pub const DEFAULT_DESCRIPTION: &str = "Press ESC to close";
/// The dialog's title (and the entry's accessible label).
pub const TITLE: &str = "Run a Command";

/// GNOME's `HistoryManager` (`js/misc/history.js`), quirks included:
/// walking away from a typed line stores it at the current slot so
/// walking back finds it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    items: Vec<String>,
    index: usize,
    limit: usize,
}

impl History {
    pub fn new(items: Vec<String>, limit: usize) -> Self {
        let index = items.len();
        Self {
            items,
            index,
            limit,
        }
    }

    /// The saved list (what goes back to GSettings).
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// GSettings changed under us (`_historyChanged`).
    pub fn replace(&mut self, items: Vec<String>) {
        self.index = items.len();
        self.items = items;
    }

    fn current(&self) -> String {
        self.items.get(self.index).cloned().unwrap_or_default()
    }

    fn store(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.items.get_mut(self.index) {
            Some(slot) => *slot = text.to_owned(),
            None => self.items.push(text.to_owned()),
        }
    }

    /// Up (`_setPrevItem`): the older entry to show, or `None` at the top.
    pub fn prev(&mut self, text: &str) -> Option<String> {
        if self.index == 0 {
            return None;
        }
        self.store(text);
        self.index -= 1;
        Some(self.current())
    }

    /// Down (`_setNextItem`): the newer entry to show, or `None` at the end.
    pub fn next(&mut self, text: &str) -> Option<String> {
        if self.index >= self.items.len() {
            return None;
        }
        self.store(text);
        self.index += 1;
        Some(self.current())
    }

    /// Back past the newest entry (`lastItem`), as every open does.
    pub fn reset(&mut self) {
        self.index = self.items.len();
    }

    /// Record a run line (`addItem`): trimmed, moved to the end without
    /// duplicates, capped at the limit. Returns the trimmed line and
    /// whether the list changed (and needs saving).
    pub fn add(&mut self, input: &str) -> (String, bool) {
        let input = input.trim().to_owned();
        let changed = !input.is_empty() && self.items.last() != Some(&input);
        if changed {
            self.items.retain(|entry| *entry != input);
            self.items.push(input.clone());
            if self.items.len() > self.limit {
                let excess = self.items.len() - self.limit;
                self.items.drain(..excess);
            }
        }
        self.index = self.items.len();
        (input, changed)
    }
}

/// The word Tab completes: everything after the last space
/// (`RunDialog` key handler).
pub fn completion_prefix(text: &str) -> &str {
    text.rsplit_once(' ').map_or(text, |(_, word)| word)
}

/// Command completion (`_getCommandCompletion`): the common prefix of
/// every name starting with `text` in `dirs`, minus `text`; `None` when
/// nothing matches.
pub fn command_completion(text: &str, dirs: &[PathBuf]) -> Option<String> {
    let mut common: Option<String> = None;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(text) {
                continue;
            }
            common = Some(match common {
                None => name.to_owned(),
                Some(prefix) => {
                    let shared = prefix
                        .char_indices()
                        .zip(name.chars())
                        .find(|((_, a), b)| a != b)
                        .map_or(prefix.len().min(name.len()), |((i, _), _)| i);
                    prefix[..shared].to_owned()
                }
            });
        }
    }
    common.map(|common| common.get(text.len()..).unwrap_or_default().to_owned())
}

/// Where command completion looks: `PATH`, then the home directory.
fn completion_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.push(glib::home_dir());
    dirs
}

/// The file a failed command line may name instead (`_run`'s fallback):
/// an absolute path, else relative to `home` with one leading `~`
/// dropped. `None` for an empty line.
pub fn fallback_path(input: &str, home: &Path) -> Option<PathBuf> {
    if input.starts_with('/') {
        return Some(PathBuf::from(input));
    }
    if input.is_empty() {
        return None;
    }
    let rest = input.strip_prefix('~').unwrap_or(input);
    Some(PathBuf::from(format!("{}/{rest}", home.display())))
}

/// The command line Ctrl+Return runs: `"<exec> <exec-arg> <input>"`.
pub fn terminal_command(exec: &str, exec_arg: &str, input: &str) -> String {
    format!("{exec} {exec_arg} {input}")
}

/// Spawn a command line (`Util.trySpawnCommandLine`): GLib's shell
/// parsing, then a `PATH` search with the session's application
/// environment. Errors read as GNOME shows them: "Command not found"
/// for a missing program, the bare system reason otherwise, GLib's own
/// text for an unparsable line.
pub fn try_spawn_command_line(command: &str) -> Result<(), String> {
    let argv = glib::shell_parse_argv(command).map_err(|e| e.message().to_owned())?;
    let (program, args) = argv.split_first().ok_or("Text was empty")?;
    let child = tuna_shell_host::apps::application_command(program)
        .args(args)
        .spawn()
        .map_err(|e| spawn_error_text(&e))?;
    // GNOME keeps the child (no double fork) and reaps it with a dummy
    // watch, so polkit sees the real parent.
    if let Ok(pid) = i32::try_from(child.id()) {
        glib::child_watch_add_local(glib::Pid(pid), |_, _| {});
    }
    Ok(())
}

/// GNOME's spawn error text: "Command not found" for `ENOENT`, else the
/// system's reason without Rust's "(os error N)" suffix.
pub fn spawn_error_text(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        return "Command not found".to_owned();
    }
    let text = error.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

fn schema(id: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(id, true)?;
    Some(gio::Settings::new(id))
}

/// The dialog.
pub struct RunDialog {
    window: gtk::Window,
    fade: crate::transient::ModalFade,
    entry: gtk::Entry,
    description: gtk::Label,
    command_error: Cell<bool>,
    history: RefCell<History>,
    shell_settings: Option<gio::Settings>,
    completer: gio::FilenameCompleter,
}

impl RunDialog {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some(TITLE));
        window.add_css_class("tuna-end-session");
        window.add_css_class("tuna-run-dialog");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("tuna-run-dialog"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
        card.add_css_class("modal-dialog");
        card.add_css_class("run-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(Some(TITLE));
        title.add_css_class("message-dialog-title");
        let entry = gtk::Entry::new();
        entry.add_css_class("run-dialog-entry");
        entry.set_input_purpose(gtk::InputPurpose::Terminal);
        entry.update_property(&[gtk::accessible::Property::Label(TITLE)]);
        let description = gtk::Label::new(Some(DEFAULT_DESCRIPTION));
        description.add_css_class("run-dialog-description");
        description.set_wrap(true);
        content.append(&title);
        content.append(&entry);
        content.append(&description);
        card.append(&content);
        window.set_child(Some(&card));

        let shell_settings = schema(SHELL_SCHEMA)
            .filter(|s| s.settings_schema().is_some_and(|s| s.has_key(HISTORY_KEY)));
        let items = shell_settings
            .as_ref()
            .map(|s| s.strv(HISTORY_KEY).iter().map(|v| v.to_string()).collect())
            .unwrap_or_default();
        let completer = gio::FilenameCompleter::new();
        completer.set_dirs_only(false);
        let ui = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&window),
            window,
            entry,
            description,
            command_error: Cell::new(false),
            history: RefCell::new(History::new(items, HISTORY_LIMIT)),
            shell_settings,
            completer,
        });
        if let Some(settings) = ui.shell_settings.as_ref() {
            let weak = Rc::downgrade(&ui);
            settings.connect_changed(Some(HISTORY_KEY), move |s, _| {
                if let Some(ui) = weak.upgrade() {
                    let items = s.strv(HISTORY_KEY).iter().map(|v| v.to_string()).collect();
                    ui.history.borrow_mut().replace(items);
                }
            });
        }
        {
            let description = ui.description.clone();
            ui.entry.connect_changed(move |_| {
                description.set_text(DEFAULT_DESCRIPTION);
            });
        }
        // Keys on the entry before GtkText sees them: Tab completes,
        // Return runs, Up/Down walk the history.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                use gtk::gdk::Key;
                match key {
                    Key::Tab | Key::KP_Tab | Key::ISO_Left_Tab => {
                        ui.complete();
                        glib::Propagation::Stop
                    }
                    Key::Return | Key::KP_Enter | Key::ISO_Enter => {
                        let in_terminal = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                        ui.run(&ui.entry.text(), in_terminal);
                        if !ui.command_error.get() {
                            ui.close();
                        }
                        glib::Propagation::Stop
                    }
                    Key::Up | Key::KP_Up => {
                        let text = ui.entry.text().trim().to_owned();
                        let shown = ui.history.borrow_mut().prev(&text);
                        ui.show_history(shown)
                    }
                    Key::Down | Key::KP_Down => {
                        let text = ui.entry.text().trim().to_owned();
                        let shown = ui.history.borrow_mut().next(&text);
                        ui.show_history(shown)
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        ui.entry.add_controller(keys);
        // Escape closes, wherever the focus is.
        let escape = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            escape.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(ui) = weak.upgrade() {
                        ui.close();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        ui.window.add_controller(escape);
        {
            let weak = Rc::downgrade(&ui);
            ui.window.connect_close_request(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.close();
                }
                glib::Propagation::Stop
            });
        }
        ui
    }

    fn show_history(&self, shown: Option<String>) -> glib::Propagation {
        match shown {
            Some(text) => {
                self.entry.set_text(&text);
                self.entry.set_position(-1);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    /// Open the dialog (`RunDialog.open()`): history back at its end, an
    /// empty line, no error — and nothing at all while
    /// `disable-command-line` denies. Returns whether it opened.
    pub fn open(&self) -> bool {
        self.history.borrow_mut().reset();
        self.entry.set_text("");
        self.command_error.set(false);
        if !crate::lockdown::read().entry_visibility().run_command {
            eprintln!("tuna-shell-gtk: run dialog refused by administrator policy");
            return false;
        }
        self.fade.present();
        self.entry.grab_focus();
        true
    }

    pub fn close(&self) {
        self.fade.hide();
    }

    fn complete(&self) {
        let text = self.entry.text().to_string();
        let prefix = completion_prefix(&text);
        let postfix = if prefix.contains('/') {
            self.completer
                .completion_suffix(prefix)
                .map(|s| s.to_string())
        } else {
            command_completion(prefix, &completion_dirs())
        };
        if let Some(postfix) = postfix.filter(|p| !p.is_empty()) {
            let mut end = self.entry.text_length().into();
            self.entry.insert_text(&postfix, &mut end);
            self.entry.set_position(end);
        }
    }

    fn show_error(&self, message: &str) {
        self.command_error.set(true);
        self.description.set_text(message);
    }

    /// `_run`: record the line, spawn it, else open the file it names,
    /// else show why in the caption.
    fn run(&self, input: &str, in_terminal: bool) {
        let (input, changed) = self.history.borrow_mut().add(input);
        if changed {
            if let Some(settings) = self.shell_settings.as_ref() {
                let items = self.history.borrow().items().to_vec();
                let items: Vec<&str> = items.iter().map(String::as_str).collect();
                let _ = settings.set_strv(HISTORY_KEY, items.as_slice());
            }
        }
        self.command_error.set(false);
        let command = if in_terminal {
            let (exec, arg) = schema(TERMINAL_SCHEMA)
                .map(|s| {
                    (
                        s.string("exec").to_string(),
                        s.string("exec-arg").to_string(),
                    )
                })
                .unwrap_or_default();
            terminal_command(&exec, &arg, &input)
        } else {
            input.clone()
        };
        let Err(error) = try_spawn_command_line(&command) else {
            return;
        };
        match fallback_path(&input, &glib::home_dir()).filter(|path| path.exists()) {
            Some(path) => {
                let uri = gio::File::for_path(&path).uri();
                let context = tuna_shell_host::apps::application_launch_context();
                if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, Some(&context)) {
                    self.show_error(e.message());
                }
            }
            None => self.show_error(&error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_walks_and_keeps_the_typed_line_like_gnome() {
        let mut h = History::new(vec!["a".into(), "b".into()], HISTORY_LIMIT);
        // Up from a typed draft stores it at the end slot.
        assert_eq!(h.prev("draft").as_deref(), Some("b"));
        assert_eq!(h.prev("b").as_deref(), Some("a"));
        assert_eq!(h.prev("a"), None, "top of the history");
        assert_eq!(h.next("a").as_deref(), Some("b"));
        assert_eq!(h.next("b").as_deref(), Some("draft"));
        // One past the stored draft is GNOME's empty line, then the end.
        assert_eq!(h.next("draft").as_deref(), Some(""));
        assert_eq!(h.next(""), None, "end of the history");
        // Reset returns to the end; adding moves duplicates to the end.
        h.reset();
        let (line, changed) = h.add("  a  ");
        assert_eq!(line, "a");
        assert!(changed);
        assert_eq!(h.items(), ["b", "draft", "a"]);
        // Re-adding the newest entry or an empty line changes nothing.
        assert!(!h.add("a").1);
        assert!(!h.add("   ").1);
    }

    #[test]
    fn history_caps_at_its_limit() {
        let mut h = History::new(vec!["1".into(), "2".into()], 2);
        h.add("3");
        assert_eq!(h.items(), ["2", "3"]);
    }

    #[test]
    fn completion_takes_the_last_word_and_the_common_prefix() {
        assert_eq!(completion_prefix("gedit /tmp/fo"), "/tmp/fo");
        assert_eq!(completion_prefix("xte"), "xte");
        let dir = tempfile::tempdir().unwrap();
        for name in ["tuna-alpha", "tuna-alps", "other"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let dirs = [dir.path().to_path_buf(), dir.path().join("missing")];
        assert_eq!(command_completion("tu", &dirs).as_deref(), Some("na-alp"));
        assert_eq!(command_completion("tuna-alph", &dirs).as_deref(), Some("a"));
        assert_eq!(command_completion("zzz", &dirs), None);
    }

    #[test]
    fn failed_lines_fall_back_to_files_like_gnome() {
        let home = Path::new("/home/u");
        assert_eq!(
            fallback_path("/etc/os-release", home),
            Some("/etc/os-release".into())
        );
        assert_eq!(
            fallback_path("Documents", home),
            Some("/home/u/Documents".into())
        );
        assert_eq!(
            fallback_path("~/notes.txt", home),
            Some("/home/u//notes.txt".into())
        );
        assert_eq!(fallback_path("", home), None);
        assert_eq!(
            terminal_command("x-terminal-emulator", "-e", "top"),
            "x-terminal-emulator -e top"
        );
    }

    #[test]
    fn spawn_errors_read_as_gnome_shows_them() {
        assert_eq!(
            try_spawn_command_line("tuna-no-such-command-357").unwrap_err(),
            "Command not found"
        );
        // Unbalanced quotes: GLib's parser text, as GNOME shows it.
        assert!(!try_spawn_command_line("echo \"open")
            .unwrap_err()
            .is_empty());
        let denied = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(spawn_error_text(&denied), "Permission denied");
    }
}
