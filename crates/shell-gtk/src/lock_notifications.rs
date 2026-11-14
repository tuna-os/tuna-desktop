//! The lock screen's notifications: GNOME 51's `NotificationsBox`
//! (js/ui/unlockDialog.js), under the clock and the prompt.
//!
//! One box per source with unseen notifications, newest source on top:
//! its icon, its name and, past one, how many are unseen. A source whose
//! policy allows details (`details-in-lock-screen`, or only system-scoped
//! notifications) lists each unseen one instead, its title in bold before
//! its body. Which sources show and what they may reveal is decided by
//! the store (`tuna_shell_host::notification_policy::lock_screen`); a
//! redacted [`LockEntry`] carries no title or body, so nothing here can
//! draw or expose one.

use std::cell::RefCell;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

/// What the lock screen shows of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// The source's name (`source.title`).
    pub title: String,
    /// Its icon, serialized (`g_icon_to_string`); `None` for GNOME's
    /// `application-x-executable` fallback.
    pub icon: Option<String>,
    /// Its unseen notifications (`unseenCount`).
    pub count: usize,
    /// Any of its notifications is critical (the `critical` style).
    pub critical: bool,
    /// Titles and bodies, oldest first, only when details may show.
    pub details: Option<Vec<(String, String)>>,
}

/// GNOME's container width, `$_message_list_width` (29em at 11pt).
const WIDTH: i32 = 425;

/// One monitor's `NotificationsBox`.
pub struct NotificationsBox {
    scroller: gtk::ScrolledWindow,
    list: gtk::Box,
    shown: RefCell<Vec<LockEntry>>,
}

impl NotificationsBox {
    pub fn new() -> Self {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.add_css_class("unlock-dialog-notifications-container");
        list.set_size_request(WIDTH, -1);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        scroller.set_widget_name("unlockDialogNotifications");
        Self {
            scroller,
            list,
            shown: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    /// Show `entries`, rebuilding only when they changed. With none the
    /// box hides (`_updateVisibility`).
    pub fn show(&self, entries: &[LockEntry]) {
        if *self.shown.borrow() == entries {
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for entry in entries {
            self.list.append(&source_box(entry));
        }
        self.scroller.set_visible(!entries.is_empty());
        *self.shown.borrow_mut() = entries.to_vec();
    }
}

/// One source's box (`_makeNotificationSource` /
/// `_makeNotificationDetailedSource`).
fn source_box(entry: &LockEntry) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("unlock-dialog-notification-source");
    if entry.critical {
        row.add_css_class("critical");
    }
    let icon = match entry
        .icon
        .as_deref()
        .and_then(|icon| gio::Icon::for_string(icon).ok())
    {
        Some(gicon) => gtk::Image::from_gicon(&gicon),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    icon.add_css_class("unlock-dialog-notification-icon");
    icon.set_pixel_size(32);
    icon.set_valign(gtk::Align::Center);
    row.append(&icon);
    let label = |text: &str, class: &str| {
        let label = gtk::Label::new(Some(text));
        label.add_css_class(class);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label
    };
    match &entry.details {
        None => {
            let text = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            text.set_hexpand(true);
            text.set_valign(gtk::Align::Center);
            let title = label(&entry.title, "unlock-dialog-notification-label");
            title.set_hexpand(true);
            text.append(&title);
            let count = gtk::Label::new(Some(&entry.count.to_string()));
            count.add_css_class("unlock-dialog-notification-count-text");
            count.set_visible(entry.count > 1);
            text.append(&count);
            row.append(&text);
        }
        Some(details) => {
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.append(&label(
                &one_line(&entry.title),
                "unlock-dialog-notification-label",
            ));
            for (title, body) in details {
                let line = label("", "unlock-dialog-notification-count-text");
                line.set_markup(&detail_markup(title, body));
                text.append(&line);
            }
            row.append(&text);
        }
    }
    row
}

/// Newlines read as spaces (`replace(/\n/g, ' ')`).
fn one_line(text: &str) -> String {
    text.replace('\n', " ")
}

/// One detailed notification: `<b>title</b> body`, on one line. Bodies
/// are plain text here, as on the shell's banners, so both are escaped.
fn detail_markup(title: &str, body: &str) -> String {
    let title = glib::markup_escape_text(title);
    if body.is_empty() {
        format!("<b>{title}</b> ")
    } else {
        format!(
            "<b>{title}</b> {}",
            glib::markup_escape_text(&one_line(body))
        )
    }
}

/// Whether going from `before` to `after` wakes a blanked lock screen
/// (unlockDialog.js `_wakeUpScreenForSource`): a source box that newly
/// shows (`_addSource`, `_visibleChanged`) or whose unseen count changed
/// while it shows (`_countChanged`). A source is its name and icon, as
/// the box draws it. The caller checks `show-banners`.
pub fn wakes_screen(before: &[LockEntry], after: &[LockEntry]) -> bool {
    after.iter().any(|entry| {
        !before.iter().any(|old| {
            old.title == entry.title && old.icon == entry.icon && old.count == entry.count
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_lines_escape_and_flatten_like_gnome() {
        assert_eq!(
            detail_markup("A <b>", "line one\nline & two"),
            "<b>A &lt;b&gt;</b> line one line &amp; two"
        );
        assert_eq!(detail_markup("Only", ""), "<b>Only</b> ");
    }

    #[test]
    fn a_new_or_recounted_source_wakes_the_screen_like_gnome() {
        let entry = |title: &str, count| LockEntry {
            title: title.into(),
            icon: None,
            count,
            critical: false,
            details: None,
        };
        let mail = [entry("Mail", 1)];
        // `_addSource` (not initial): a source box appears.
        assert!(wakes_screen(&[], &mail));
        // `_countChanged`: its count moves, up or down, while it shows.
        assert!(wakes_screen(&mail, &[entry("Mail", 2)]));
        assert!(wakes_screen(&[entry("Mail", 2)], &mail));
        // Nothing that shows changed, or a source left: no wake.
        assert!(!wakes_screen(&mail, &mail));
        assert!(!wakes_screen(&[entry("Chat", 1), entry("Mail", 1)], &mail));
        assert!(!wakes_screen(&mail, &[]));
        // Details or the critical style alone are no new notification.
        let detailed = LockEntry {
            details: Some(vec![("Hi".into(), String::new())]),
            critical: true,
            ..entry("Mail", 1)
        };
        assert!(!wakes_screen(&mail, &[detailed]));
    }
}
