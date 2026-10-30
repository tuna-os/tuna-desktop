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
//! draw or expose one. Above the sources sit the media players
//! (`MediaMessage`s, [`MediaEntry`]), as GNOME's box lists them first.

use std::cell::RefCell;
use std::rc::Rc;

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

/// What the lock screen shows of one media player (messageList.js
/// `MediaMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEntry {
    /// The player's bus name, which the controls act on.
    pub bus_name: String,
    /// Its app's name, else its MPRIS `Identity` (the message's source).
    pub source: String,
    /// Its app's icon, serialized.
    pub source_icon: Option<String>,
    /// The track title.
    pub title: String,
    /// The track's artists, comma-separated.
    pub body: String,
    /// The cover's URI, if the track has one.
    pub cover_url: Option<String>,
    /// `PlaybackStatus` is `Playing`: the middle control pauses.
    pub playing: bool,
    pub can_go_previous: bool,
    pub can_go_next: bool,
}

/// A `MediaMessage` control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    Previous,
    PlayPause,
    Next,
}

/// Drives a player, by its bus name, from its `MediaMessage`'s controls.
pub type MediaActions = Rc<dyn Fn(&str, MediaAction)>;

/// GNOME's container width, `$_message_list_width` (29em at 11pt).
const WIDTH: i32 = 425;

/// One monitor's `NotificationsBox`.
pub struct NotificationsBox {
    scroller: gtk::ScrolledWindow,
    list: gtk::Box,
    /// The players' `MediaMessage`s, above the sources.
    media: gtk::Box,
    sources: gtk::Box,
    shown: RefCell<Vec<LockEntry>>,
    shown_media: RefCell<Vec<MediaEntry>>,
}

impl NotificationsBox {
    pub fn new() -> Self {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.add_css_class("unlock-dialog-notifications-container");
        list.set_size_request(WIDTH, -1);
        let media = gtk::Box::new(gtk::Orientation::Vertical, 6);
        media.set_visible(false);
        list.append(&media);
        let sources = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.append(&sources);
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
            media,
            sources,
            shown: RefCell::default(),
            shown_media: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    /// Reads how tall the box would draw with nothing scrolled away (its
    /// preferred height at the width it draws), 0 while hidden.
    pub fn natural_height(&self) -> impl Fn() -> i32 + 'static {
        let (scroller, list) = (self.scroller.clone(), self.list.clone());
        move || {
            if !scroller.is_visible() {
                return 0;
            }
            let width = scroller.width().max(WIDTH);
            let (_, natural, _, _) = list.measure(gtk::Orientation::Vertical, width);
            natural
        }
    }

    /// Show `entries`, rebuilding only when they changed. With neither
    /// sources nor players the box hides (`_updateVisibility`).
    pub fn show(&self, entries: &[LockEntry]) {
        if *self.shown.borrow() == entries {
            return;
        }
        while let Some(child) = self.sources.first_child() {
            self.sources.remove(&child);
        }
        for entry in entries {
            self.sources.append(&source_box(entry));
        }
        *self.shown.borrow_mut() = entries.to_vec();
        self.update_visibility();
    }

    /// Show the media players above the sources (`_addPlayer` inserts each
    /// at index 0), rebuilding only when they changed; `act` drives the
    /// named player.
    pub fn show_media(&self, players: &[MediaEntry], act: &MediaActions) {
        if *self.shown_media.borrow() == players {
            return;
        }
        while let Some(child) = self.media.first_child() {
            self.media.remove(&child);
        }
        for player in players {
            self.media.append(&media_message(player, act));
        }
        self.media.set_visible(!players.is_empty());
        *self.shown_media.borrow_mut() = players.to_vec();
        self.update_visibility();
    }

    fn update_visibility(&self) {
        self.scroller
            .set_visible(!self.shown.borrow().is_empty() || !self.shown_media.borrow().is_empty());
    }
}

/// One player's `MediaMessage`: the header names its app, the cover (or
/// `audio-x-generic-symbolic`) beside the track title and artists, and
/// GNOME's three media controls. Clicking the message itself does
/// nothing while locked (`vfunc_clicked`).
fn media_message(player: &MediaEntry, act: &MediaActions) -> gtk::Box {
    let message = gtk::Box::new(gtk::Orientation::Vertical, 0);
    message.add_css_class("message");
    message.add_css_class("media-message");
    message.set_accessible_role(gtk::AccessibleRole::Group);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.add_css_class("message-header");
    if let Some(icon) = player
        .source_icon
        .as_deref()
        .and_then(|icon| gio::Icon::for_string(icon).ok())
    {
        let image = gtk::Image::from_gicon(&icon);
        image.set_pixel_size(16);
        header.append(&image);
    }
    let source = gtk::Label::new(Some(&player.source));
    source.add_css_class("message-source-title");
    source.set_xalign(0.0);
    source.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header.append(&source);
    message.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.add_css_class("message-box");
    let cover = match &player.cover_url {
        Some(url) => {
            let image = gtk::Image::from_gicon(&gio::FileIcon::new(&gio::File::for_uri(url)));
            image.set_pixel_size(48);
            image
        }
        None => {
            let image = gtk::Image::from_icon_name("audio-x-generic-symbolic");
            image.add_css_class("message-themed-icon");
            image.set_pixel_size(32);
            image
        }
    };
    cover.add_css_class("message-icon");
    cover.set_valign(gtk::Align::Start);
    content.append(&cover);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.add_css_class("message-content");
    text.set_hexpand(true);
    for (line, class) in [
        (&player.title, "message-title"),
        (&player.body, "message-body"),
    ] {
        let label = gtk::Label::new(Some(&one_line(line)));
        label.add_css_class(class);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&label);
    }
    content.append(&text);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.set_valign(gtk::Align::Center);
    let play_icon = if player.playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    };
    for (icon, name, action, sensitive) in [
        (
            "media-skip-backward-symbolic",
            "Previous",
            MediaAction::Previous,
            player.can_go_previous,
        ),
        (
            play_icon,
            if player.playing { "Pause" } else { "Play" },
            MediaAction::PlayPause,
            true,
        ),
        (
            "media-skip-forward-symbolic",
            "Next",
            MediaAction::Next,
            player.can_go_next,
        ),
    ] {
        let button = gtk::Button::from_icon_name(icon);
        button.add_css_class("message-media-control");
        button.add_css_class("flat");
        button.set_sensitive(sensitive);
        button.update_property(&[gtk::accessible::Property::Label(name)]);
        let (act, bus_name) = (act.clone(), player.bus_name.clone());
        button.connect_clicked(move |_| act(&bus_name, action));
        controls.append(&button);
    }
    content.append(&controls);
    message.append(&content);
    message
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
}
