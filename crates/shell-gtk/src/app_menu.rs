//! GNOME 51's app-icon menu (js/ui/appMenu.js), opened from an app's
//! icon in the dash, the app grid or the search results
//! (appDisplay.js `AppIcon.popupMenu`, :3020) by a right click, a long
//! press, or Menu / Shift+F10 on the focused icon.
//!
//! The menu lists, in GNOME's order and under GNOME's conditions: the
//! app's open windows under an "Open Windows" heading, New Window (when
//! the app can open one and its desktop file has no `new-window`
//! action of its own), its desktop actions, the launch on the other
//! graphics card (a stopped app, with switcheroo-control reporting two
//! GPUs), Pin to Dash or Unpin, App Details (when GNOME Software is
//! installed) and Quit (a running app). A separator shows only between
//! two visible entries (`_updateSeparatorVisibility`).
//!
//! [`rows`] is that list; [`AppMenus`] gathers what it depends on, draws
//! it as a popover in the panel menus' style and carries out the row
//! chosen.

use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_shell_host::apps::AppEntry;

use crate::app_favorites::{desktop_id, AppFavorites};
use crate::overview::OverviewActions;
use crate::switcheroo::Switcheroo;
use gtk4_layer_shell::{KeyboardMode, LayerShell};

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `Main.activateWindow`: raise this window.
    Raise(u64),
    /// `app.open_new_window(-1)`.
    NewWindow,
    /// `app.launch_action(action)`: one of the desktop file's actions.
    Action(String),
    /// `app.launch(0, -1, gpu)`: `discrete` for the other graphics card.
    LaunchOnGpu { discrete: bool },
    /// Pin to Dash / Unpin.
    TogglePin,
    /// GNOME Software's details page for the app.
    Details,
    /// `app.request_quit()`.
    Quit,
}

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A labelled separator: "Open Windows".
    Heading(String),
    Separator,
    Item {
        label: String,
        command: Command,
    },
}

/// What the menu depends on (`setApp` and the `_update*` methods).
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// The app's windows `(id, title)`, titles already falling back to
    /// the app's name.
    pub windows: Vec<(u64, String)>,
    /// `state === RUNNING`.
    pub running: bool,
    /// The app has a desktop file (a window-backed app has none).
    pub has_desktop_file: bool,
    /// `app.can_open_new_window()`.
    pub can_open_new_window: bool,
    /// The desktop file's actions `(id, name)`.
    pub actions: Vec<(String, String)>,
    /// `Some(is_favorite)` when the app can be pinned: it has a desktop
    /// file and `favorite-apps` is writable.
    pub favorite: Option<bool>,
    /// switcheroo-control's `HasDualGpu`.
    pub dual_gpu: bool,
    /// The desktop file's `PrefersNonDefaultGPU`.
    pub prefers_non_default_gpu: bool,
    /// GNOME Software is installed.
    pub software: bool,
}

/// An app icon's menu: GNOME's rows for `facts`. `AppIcon.popupMenu`
/// shows a lone window too (`showSingleWindows: true`).
pub fn rows(facts: &Facts) -> Vec<Row> {
    let item = |label: &str, command| Row::Item {
        label: label.to_owned(),
        command,
    };
    let mut all: Vec<Row> = Vec::new();
    if !facts.windows.is_empty() {
        all.push(Row::Heading("Open Windows".to_owned()));
        for (id, title) in &facts.windows {
            all.push(item(title, Command::Raise(*id)));
        }
    }
    all.push(Row::Separator);
    let has_new_window_action = facts.actions.iter().any(|(id, _)| id == "new-window");
    if facts.can_open_new_window && !has_new_window_action {
        all.push(item("New Window", Command::NewWindow));
    }
    for (id, name) in &facts.actions {
        all.push(item(name, Command::Action(id.clone())));
    }
    if !facts.running && facts.has_desktop_file && facts.dual_gpu {
        // The card the app does not prefer, as `_getNonDefaultLaunchGpu`.
        let discrete = !facts.prefers_non_default_gpu;
        all.push(item(
            if discrete {
                "Launch Using Discrete Graphics Card"
            } else {
                "Launch Using Integrated Graphics Card"
            },
            Command::LaunchOnGpu { discrete },
        ));
    }
    all.push(Row::Separator);
    if let Some(favorite) = facts.favorite {
        all.push(item(
            if favorite { "Unpin" } else { "Pin to Dash" },
            Command::TogglePin,
        ));
    }
    all.push(Row::Separator);
    if facts.software && facts.has_desktop_file {
        all.push(item("App Details", Command::Details));
    }
    all.push(Row::Separator);
    if facts.running {
        all.push(item("Quit", Command::Quit));
    }
    collapse_separators(all)
}

/// `_updateSeparatorVisibility`: a separator shows only with a visible
/// entry before it and another after it; of several in a row one stays.
fn collapse_separators(all: Vec<Row>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(all.len());
    for row in all {
        if row == Row::Separator && !matches!(rows.last(), Some(last) if *last != Row::Separator) {
            continue;
        }
        rows.push(row);
    }
    if rows.last() == Some(&Row::Separator) {
        rows.pop();
    }
    rows
}

/// The app an icon stands for.
#[derive(Clone)]
pub struct Subject {
    /// Its desktop entry; `None` for a window-backed app.
    pub entry: Option<AppEntry>,
    /// The window a window-backed app is.
    pub window: Option<u64>,
}

/// The icons' menus: builds each from the facts when it opens and runs
/// the row chosen.
pub struct AppMenus {
    actions: Rc<dyn OverviewActions>,
    favorites: Rc<AppFavorites>,
    switcheroo: Rc<Switcheroo>,
    /// The open menu and its icon.
    open: RefCell<Option<(gtk::Popover, gtk::Button)>>,
}

const SOFTWARE: &str = "org.gnome.Software";

impl AppMenus {
    pub fn new(
        actions: Rc<dyn OverviewActions>,
        favorites: Rc<AppFavorites>,
        switcheroo: Rc<Switcheroo>,
    ) -> Rc<Self> {
        Rc::new(Self {
            actions,
            favorites,
            switcheroo,
            open: RefCell::new(None),
        })
    }

    /// Open `tile`'s menu on a right click, a long press, or Menu or
    /// Shift+F10 while it has the focus. `side` is where the menu goes.
    pub fn attach(
        self: &Rc<Self>,
        tile: &gtk::Button,
        side: gtk::PositionType,
        subject: impl Fn() -> Subject + 'static,
    ) {
        let subject = Rc::new(subject);
        let open = {
            let (menus, subject) = (Rc::downgrade(self), subject.clone());
            move |tile: &gtk::Button, keyboard: bool| {
                if let Some(menus) = menus.upgrade() {
                    menus.popup(tile, side, &subject(), keyboard);
                }
            }
        };
        // The secondary button recognizes on press (`recognize_on_press`).
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        {
            let (open, weak) = (open.clone(), tile.downgrade());
            click.connect_pressed(move |gesture, _, _, _| {
                if let Some(tile) = weak.upgrade() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    open(&tile, false);
                }
            });
        }
        tile.add_controller(click);
        let press = gtk::GestureLongPress::new();
        {
            let (open, weak) = (open.clone(), tile.downgrade());
            press.connect_pressed(move |gesture, _, _| {
                if let Some(tile) = weak.upgrade() {
                    // Claimed: the press that opened the menu is no click.
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    open(&tile, false);
                }
            });
        }
        tile.add_controller(press);
        let weak = tile.downgrade();
        crate::on_popup_menu_key(tile, move || {
            let Some(tile) = weak.upgrade() else {
                return false;
            };
            open(&tile, true);
            true
        });
    }

    /// Close the open menu at once (the overview hides, or its icons
    /// are rebuilt).
    pub fn close(&self) {
        let Some((popover, tile)) = self.open.borrow_mut().take() else {
            return;
        };
        tile.remove_css_class("menu-open");
        popover.popdown();
        popover.unparent();
    }

    fn facts(&self, subject: &Subject) -> Facts {
        let mut windows: Vec<u64> = match (&subject.entry, subject.window) {
            (_, Some(window)) => vec![window],
            (Some(entry), None) => {
                let app_id = entry.app_id.trim_end_matches(".desktop");
                self.actions
                    .running()
                    .into_iter()
                    .filter(|(_, app)| app.as_deref() == Some(app_id))
                    .map(|(id, _)| id)
                    .collect()
            }
            (None, None) => Vec::new(),
        };
        windows.sort_unstable();
        let name = subject
            .entry
            .as_ref()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        let titled: Vec<(u64, String)> = windows
            .iter()
            .map(|&id| {
                let title = self.actions.window_title(id);
                (
                    id,
                    if title.is_empty() {
                        name.clone()
                    } else {
                        title
                    },
                )
            })
            .collect();
        let running = !windows.is_empty();
        let menu = subject
            .entry
            .as_ref()
            .map(|entry| tuna_shell_host::apps::desktop_menu(&entry.app_id))
            .unwrap_or_default();
        Facts {
            windows: titled,
            running,
            has_desktop_file: subject.entry.is_some(),
            // A stopped app can always open one (activating it does);
            // a running one is the desktop file's say.
            can_open_new_window: match &subject.entry {
                Some(entry) => !running || entry.can_open_new_window(),
                None => false,
            },
            actions: menu
                .actions
                .into_iter()
                .map(|action| (action.id, action.name))
                .collect(),
            favorite: subject
                .entry
                .as_ref()
                .filter(|_| self.favorites.writable())
                .map(|entry| self.favorites.is_favorite(&desktop_id(&entry.app_id))),
            dual_gpu: self.switcheroo.has_dual_gpu(),
            prefers_non_default_gpu: menu.prefers_non_default_gpu,
            software: tuna_shell_host::apps::desktop_file_exists(SOFTWARE),
        }
    }

    /// `AppIcon.popupMenu`: open `tile`'s menu; from the keyboard its
    /// first row takes the focus (`navigate_focus`).
    pub fn popup(
        self: &Rc<Self>,
        tile: &gtk::Button,
        side: gtk::PositionType,
        subject: &Subject,
        keyboard: bool,
    ) {
        self.close();
        let rows = rows(&self.facts(subject));
        if rows.is_empty() {
            return;
        }
        let popover = crate::transient::MenuPopover::new_popover();
        popover.add_css_class("tuna-shell-popover");
        popover.add_css_class("tuna-app-menu");
        popover.set_has_arrow(false);
        popover.set_position(side);
        let run: Rc<dyn Fn(&Command)> = {
            let (menus, subject, tile) = (Rc::downgrade(self), subject.clone(), tile.downgrade());
            Rc::new(move |command| {
                if let (Some(menus), Some(tile)) = (menus.upgrade(), tile.upgrade()) {
                    menus.run(command, &subject, &tile);
                }
            })
        };
        let (content, first) = menu_content(&rows, &run, &popover);
        popover.set_child(Some(&content));
        // The grid and dash surfaces take no keyboard of their own; the
        // open menu does, as the window menu's surface does.
        let window = tile.root().and_downcast::<gtk::ApplicationWindow>();
        let mode = window.as_ref().map(|window| {
            let mode = window.keyboard_mode();
            window.set_keyboard_mode(KeyboardMode::Exclusive);
            mode
        });
        popover.set_parent(tile);
        tile.add_css_class("menu-open");
        *self.open.borrow_mut() = Some((popover.clone(), tile.clone()));
        {
            let (menus, tile) = (Rc::downgrade(self), tile.downgrade());
            popover.connect_closed(move |popover| {
                if let (Some(window), Some(mode)) = (window.as_ref(), mode) {
                    window.set_keyboard_mode(mode);
                }
                if let Some(tile) = tile.upgrade() {
                    tile.remove_css_class("menu-open");
                    // Back to the icon, as GNOME's menu manager hands it.
                    if keyboard {
                        tile.grab_focus();
                    }
                }
                let popover = popover.clone();
                if let Some(menus) = menus.upgrade() {
                    let mut open = menus.open.borrow_mut();
                    if open.as_ref().is_some_and(|(p, _)| *p == popover) {
                        open.take();
                    }
                }
                glib::idle_add_local_once(move || popover.unparent());
            });
        }
        popover.popup();
        if keyboard {
            if let Some(first) = first {
                first.grab_focus();
            }
            if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
                window.set_focus_visible(true);
            }
        }
    }

    fn run(&self, command: &Command, subject: &Subject, tile: &gtk::Button) {
        let entry = subject.entry.as_ref();
        match (command, entry) {
            (Command::Raise(window), _) => {
                self.actions.activate_window(*window);
                self.actions.close_overview();
            }
            (Command::NewWindow, Some(entry)) => {
                // `_animateLaunch`, `app.open_new_window(-1)`,
                // `Main.overview.hide()`.
                crate::grid_animation::launch_zoom(tile);
                if let Err(e) = tuna_shell_host::apps::open_new_window(entry) {
                    eprintln!("tuna-shell-gtk: new window {}: {e}", entry.app_id);
                }
                self.actions.close_overview();
            }
            (Command::Action(action), Some(entry)) => {
                if action == "new-window" {
                    crate::grid_animation::launch_zoom(tile);
                }
                let found = tuna_shell_host::apps::desktop_menu(&entry.app_id)
                    .actions
                    .into_iter()
                    .find(|candidate| candidate.id == *action);
                match found {
                    Some(found) => {
                        if let Err(e) = tuna_shell_host::apps::launch_action(&entry.app_id, &found)
                        {
                            eprintln!("tuna-shell-gtk: action {action}: {e}");
                        }
                    }
                    None => {
                        eprintln!("tuna-shell-gtk: action {action}: {} lost it", entry.app_id)
                    }
                }
                self.actions.close_overview();
            }
            (Command::LaunchOnGpu { discrete }, Some(entry)) => {
                crate::grid_animation::launch_zoom(tile);
                let environment = if *discrete {
                    self.switcheroo.discrete_environment().unwrap_or_default()
                } else {
                    Vec::new()
                };
                if let Err(e) = tuna_shell_host::apps::launch_with_environment(entry, &environment)
                {
                    eprintln!("tuna-shell-gtk: launch {}: {e}", entry.app_id);
                }
                self.actions.close_overview();
            }
            (Command::TogglePin, Some(entry)) => {
                let id = desktop_id(&entry.app_id);
                if self.favorites.is_favorite(&id) {
                    self.favorites.remove(&id);
                } else {
                    self.favorites.add_at(&id, None);
                }
            }
            (Command::Details, Some(entry)) => {
                open_details(&desktop_id(&entry.app_id));
                self.actions.close_overview();
            }
            (Command::Quit, _) => {
                // `shell_app_request_quit`: close every window.
                for (id, _) in self.facts(subject).windows {
                    self.actions.close_window(id);
                }
            }
            (_, None) => {}
        }
    }
}

/// appMenu.js's App Details: GNOME Software's `details` action.
fn open_details(id: &str) {
    let target = glib::Variant::tuple_from_iter([id.to_variant(), "".to_variant()]);
    let params = glib::Variant::tuple_from_iter([
        "details".to_variant(),
        vec![target].to_variant(),
        glib::VariantDict::new(None).end(),
    ]);
    gio::bus_get(
        gio::BusType::Session,
        gio::Cancellable::NONE,
        move |connection| {
            let Ok(connection) = connection else { return };
            connection.call(
                Some("org.gnome.Software"),
                "/org/gnome/Software",
                "org.gtk.Actions",
                "Activate",
                Some(&params),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                |reply| {
                    if let Err(e) = reply {
                        eprintln!("tuna-shell-gtk: App Details: {e}");
                    }
                },
            );
        },
    );
}

/// The popover's content: GNOME's `.popup-menu-content`, a heading as a
/// labelled separator, rows as menu items. Choosing one closes the menu
/// and runs it. Also the first row, for the keyboard.
fn menu_content(
    rows: &[Row],
    run: &Rc<dyn Fn(&Command)>,
    popover: &gtk::Popover,
) -> (gtk::Box, Option<gtk::Button>) {
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("popup-menu-content");
    menu.set_accessible_role(gtk::AccessibleRole::Menu);
    let mut first = None;
    for row in rows {
        match row {
            Row::Separator | Row::Heading(_) => {
                let separator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                separator.add_css_class("popup-separator-menu-item");
                if let Row::Heading(text) = row {
                    let label = gtk::Label::new(Some(text));
                    label.set_xalign(0.0);
                    separator.append(&label);
                    separator.add_css_class("popup-heading-menu-item");
                }
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                line.add_css_class("popup-separator-menu-item-separator");
                line.set_valign(gtk::Align::Center);
                line.set_hexpand(true);
                separator.append(&line);
                menu.append(&separator);
            }
            Row::Item { label, command } => {
                let text = gtk::Label::new(Some(label));
                text.set_xalign(0.0);
                text.set_hexpand(true);
                text.set_ellipsize(gtk::pango::EllipsizeMode::End);
                text.set_max_width_chars(40);
                let button = gtk::Button::builder()
                    .child(&text)
                    .accessible_role(gtk::AccessibleRole::MenuItem)
                    .build();
                button.add_css_class("popup-menu-item");
                button.update_property(&[gtk::accessible::Property::Label(label)]);
                let (run, popover, command) = (run.clone(), popover.clone(), command.clone());
                button.connect_clicked(move |_| {
                    popover.popdown();
                    run(&command);
                });
                if first.is_none() {
                    first = Some(button.clone());
                }
                menu.append(&button);
            }
        }
    }
    (menu, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(facts: &Facts) -> Vec<String> {
        rows(facts)
            .into_iter()
            .map(|row| match row {
                Row::Heading(text) => format!("# {text}"),
                Row::Separator => "---".to_owned(),
                Row::Item { label, .. } => label,
            })
            .collect()
    }

    fn app() -> Facts {
        Facts {
            has_desktop_file: true,
            can_open_new_window: true,
            favorite: Some(false),
            software: true,
            ..Facts::default()
        }
    }

    #[test]
    fn a_stopped_app_offers_new_window_pin_and_details() {
        assert_eq!(
            labels(&app()),
            ["New Window", "---", "Pin to Dash", "---", "App Details"]
        );
    }

    #[test]
    fn a_running_app_lists_its_windows_even_one_and_can_quit() {
        let facts = Facts {
            running: true,
            windows: vec![(4, "Inbox".into()), (9, "Draft".into())],
            favorite: Some(true),
            ..app()
        };
        assert_eq!(
            labels(&facts),
            [
                "# Open Windows",
                "Inbox",
                "Draft",
                "---",
                "New Window",
                "---",
                "Unpin",
                "---",
                "App Details",
                "---",
                "Quit"
            ]
        );
        let one = Facts {
            running: true,
            windows: vec![(4, "Inbox".into())],
            ..app()
        };
        assert_eq!(labels(&one)[..2], ["# Open Windows", "Inbox"]);
        assert_eq!(
            rows(&one)[1],
            Row::Item {
                label: "Inbox".into(),
                command: Command::Raise(4)
            }
        );
    }

    #[test]
    fn a_new_window_action_replaces_the_new_window_row() {
        let facts = Facts {
            actions: vec![
                ("new-window".into(), "New Window".into()),
                ("new-private-window".into(), "New Private Window".into()),
            ],
            ..app()
        };
        let rows = rows(&facts);
        assert_eq!(
            rows[..2],
            [
                Row::Item {
                    label: "New Window".into(),
                    command: Command::Action("new-window".into())
                },
                Row::Item {
                    label: "New Private Window".into(),
                    command: Command::Action("new-private-window".into())
                },
            ]
        );
    }

    #[test]
    fn a_single_window_app_has_no_new_window() {
        let facts = Facts {
            running: true,
            windows: vec![(1, "Calculator".into())],
            can_open_new_window: false,
            ..app()
        };
        assert!(!labels(&facts).contains(&"New Window".to_owned()));
    }

    #[test]
    fn the_other_gpu_is_for_a_stopped_app_with_two_gpus() {
        let dual = Facts {
            dual_gpu: true,
            ..app()
        };
        assert_eq!(labels(&dual)[1], "Launch Using Discrete Graphics Card");
        assert_eq!(
            rows(&dual)[1],
            Row::Item {
                label: "Launch Using Discrete Graphics Card".into(),
                command: Command::LaunchOnGpu { discrete: true }
            }
        );
        // An app that prefers the discrete card offers the integrated one.
        let prefers = Facts {
            prefers_non_default_gpu: true,
            ..dual.clone()
        };
        assert_eq!(labels(&prefers)[1], "Launch Using Integrated Graphics Card");
        // One GPU, or a running app, has no such row.
        assert!(!labels(&app()).iter().any(|l| l.contains("Graphics")));
        let running = Facts {
            running: true,
            windows: vec![(1, "Game".into())],
            ..dual
        };
        assert!(!labels(&running).iter().any(|l| l.contains("Graphics")));
    }

    #[test]
    fn pinning_needs_a_desktop_file_and_a_writable_key() {
        let unpinnable = Facts {
            favorite: None,
            ..app()
        };
        assert_eq!(labels(&unpinnable), ["New Window", "---", "App Details"]);
    }

    #[test]
    fn a_window_backed_app_has_its_windows_and_quit_only() {
        let facts = Facts {
            running: true,
            windows: vec![(7, "Untitled".into())],
            has_desktop_file: false,
            software: true,
            ..Facts::default()
        };
        assert_eq!(
            labels(&facts),
            ["# Open Windows", "Untitled", "---", "Quit"]
        );
    }

    #[test]
    fn separators_never_lead_trail_or_double() {
        let nothing = Facts::default();
        assert!(rows(&nothing).is_empty());
        // Only Details and Quit remain between two rules.
        let facts = Facts {
            running: true,
            windows: vec![(1, "A".into())],
            has_desktop_file: true,
            software: true,
            ..Facts::default()
        };
        assert_eq!(
            labels(&facts),
            ["# Open Windows", "A", "---", "App Details", "---", "Quit"]
        );
    }
}
