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

/// What one media player's `MediaMessage` (messageList.js) shows, on the
/// lock screen and in the calendar's message list.
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
            let message = MediaMessage::new(player, act, gtk::AccessibleRole::Group);
            self.media.append(&message.widget);
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

/// The middle control's icon and accessible name (`MediaMessage._update`):
/// pause while the player plays, else play.
pub fn play_pause(playing: bool) -> (&'static str, &'static str) {
    if playing {
        ("media-playback-pause-symbolic", "Pause")
    } else {
        ("media-playback-start-symbolic", "Play")
    }
}

/// One player's `MediaMessage`, on the lock screen and in the calendar's
/// message list alike: the header names its app, the cover (or
/// `audio-x-generic-symbolic`) beside the track title and artists, and
/// GNOME's three media controls, previous and next insensitive while the
/// player cannot go there. [`MediaMessage::update`] follows the player in
/// place, as `_update` does on `changed`. Clicking the message itself is
/// its owner's: nothing while locked (`vfunc_clicked`), raising the
/// player in the calendar.
pub struct MediaMessage {
    pub widget: gtk::Box,
    source_icon: gtk::Image,
    source: gtk::Label,
    cover: gtk::Image,
    title: gtk::Label,
    body: gtk::Label,
    previous: gtk::Button,
    play_pause: gtk::Button,
    next: gtk::Button,
}

impl MediaMessage {
    /// The message for `player`, its controls driving it through `act`,
    /// with the accessible `role` its list gives messages.
    pub fn new(player: &MediaEntry, act: &MediaActions, role: gtk::AccessibleRole) -> Self {
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(role)
            .build();
        widget.add_css_class("message");
        widget.add_css_class("media-message");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("message-header");
        let source_icon = gtk::Image::new();
        source_icon.add_css_class("message-source-icon");
        source_icon.set_pixel_size(16);
        header.append(&source_icon);
        let source = gtk::Label::new(None);
        source.add_css_class("message-source-title");
        source.set_xalign(0.0);
        source.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&source);
        widget.append(&header);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        content.add_css_class("message-box");
        let cover = gtk::Image::new();
        cover.add_css_class("message-icon");
        cover.set_valign(gtk::Align::Start);
        content.append(&cover);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.add_css_class("message-content");
        text.set_hexpand(true);
        let line = |class: &str| {
            let label = gtk::Label::new(None);
            label.add_css_class(class);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&label);
            label
        };
        let title = line("message-title");
        let body = line("message-body");
        content.append(&text);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls.set_valign(gtk::Align::Center);
        let control = |icon: &str, name: &str, action: MediaAction| {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("message-media-control");
            button.add_css_class("flat");
            button.update_property(&[gtk::accessible::Property::Label(name)]);
            let (act, bus_name) = (act.clone(), player.bus_name.clone());
            button.connect_clicked(move |_| act(&bus_name, action));
            controls.append(&button);
            button
        };
        let previous = control(
            "media-skip-backward-symbolic",
            "Previous",
            MediaAction::Previous,
        );
        let (icon, name) = play_pause(player.playing);
        let play_pause = control(icon, name, MediaAction::PlayPause);
        let next = control("media-skip-forward-symbolic", "Next", MediaAction::Next);
        content.append(&controls);
        widget.append(&content);
        let message = Self {
            widget,
            source_icon,
            source,
            cover,
            title,
            body,
            previous,
            play_pause,
            next,
        };
        message.update(player);
        message
    }

    /// Show `player` now (`_update`): its app, cover, track, the middle
    /// control's play or pause, and which way it can skip.
    pub fn update(&self, player: &MediaEntry) {
        match player
            .source_icon
            .as_deref()
            .and_then(|icon| gio::Icon::for_string(icon).ok())
        {
            Some(icon) => {
                self.source_icon.set_from_gicon(&icon);
                self.source_icon.set_visible(true);
            }
            None => self.source_icon.set_visible(false),
        }
        self.source.set_label(&player.source);
        match &player.cover_url {
            Some(url) => {
                self.cover
                    .set_from_gicon(&gio::FileIcon::new(&gio::File::for_uri(url)));
                self.cover.set_pixel_size(48);
                self.cover.remove_css_class("message-themed-icon");
            }
            None => {
                self.cover.set_icon_name(Some("audio-x-generic-symbolic"));
                self.cover.set_pixel_size(32);
                self.cover.add_css_class("message-themed-icon");
            }
        }
        self.title.set_label(&one_line(&player.title));
        self.body.set_label(&one_line(&player.body));
        let (icon, name) = play_pause(player.playing);
        self.play_pause.set_icon_name(icon);
        self.play_pause
            .update_property(&[gtk::accessible::Property::Label(name)]);
        self.previous.set_sensitive(player.can_go_previous);
        self.next.set_sensitive(player.can_go_next);
        self.widget.update_property(&[
            gtk::accessible::Property::Label(player.title.as_str()),
            gtk::accessible::Property::Description(player.body.as_str()),
        ]);
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
    fn play_pause_icon_follows_playback_status() {
        // `status === 'Playing'` pauses; Paused, Stopped and anything
        // else plays.
        assert_eq!(play_pause(true), ("media-playback-pause-symbolic", "Pause"));
        assert_eq!(play_pause(false), ("media-playback-start-symbolic", "Play"));
    }
}
