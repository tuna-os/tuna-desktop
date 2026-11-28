//! The prompt's steps other than the entry (gnome-shell 51
//! `js/gdm/authPrompt.js`): the choice list (`authList.js`, a ChoiceList
//! query or SSSD's certificates), the single button (`_authButton`, web
//! login's "Log In") and the web login dialog (`webLogin.js`: message,
//! QR code of the URL, the URL without its scheme, "Login code: ",
//! Cancel and Done), plus the prompt's timed login indicator.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_auth_prompt::{Ask, View};

/// webLogin.js `QR_CODE_SIZE`.
const QR_CODE_SIZE: usize = 150;
/// webLogin.js `URL_LABEL_LONG_THRESHOLD`.
const URL_LABEL_LONG_THRESHOLD: usize = 45;

/// webLogin.js `_formatURLForDisplay`: no scheme, no trailing slash.
pub fn display_url(url: &str) -> String {
    let url = url.strip_suffix('/').unwrap_or(url);
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url)
        .to_owned()
}

/// The QR code of `text` as dark modules on light, with a quiet zone,
/// scaled by whole pixels to at least [`QR_CODE_SIZE`]: RGB rows and the
/// side length.
pub fn qr_pixels(text: &str) -> Option<(Vec<u8>, usize)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2;
    let modules = width + 2 * quiet;
    let scale = QR_CODE_SIZE.div_ceil(modules);
    let side = modules * scale;
    let mut pixels = vec![255u8; side * side * 3];
    for (index, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let (mx, my) = (index % width + quiet, index / width + quiet);
        for y in my * scale..(my + 1) * scale {
            let row = y * side * 3;
            pixels[row + mx * scale * 3..row + (mx + 1) * scale * 3].fill(0);
        }
    }
    Some((pixels, side))
}

fn qr_texture(text: &str) -> Option<gtk::gdk::Texture> {
    let (pixels, side) = qr_pixels(text)?;
    let side = i32::try_from(side).ok()?;
    Some(
        gtk::gdk::MemoryTexture::new(
            side,
            side,
            gtk::gdk::MemoryFormat::R8g8b8,
            &glib::Bytes::from_owned(pixels),
            usize::try_from(side).ok()? * 3,
        )
        .upcast(),
    )
}

pub struct AuthUi {
    /// Everything below the entry row that replaces it.
    pub root: gtk::Box,
    list_box: gtk::Box,
    title: gtk::Label,
    list: gtk::ListBox,
    button: gtk::Button,
    web: gtk::Box,
    web_title: gtk::Label,
    qr: gtk::Picture,
    url: gtk::Label,
    code_box: gtk::Box,
    code: gtk::Label,
    cancel: gtk::Button,
    done: gtk::Button,
    spinner: gtk::Spinner,
    /// The prompt's `login-dialog-timed-login-indicator` (shown when
    /// the user list is disabled).
    pub timed: gtk::ProgressBar,
    shown: RefCell<Option<Ask>>,
    keys: RefCell<Vec<String>>,
}

impl AuthUi {
    pub fn new() -> Rc<Self> {
        let title = gtk::Label::new(None);
        title.add_css_class("login-dialog-auth-list-title-label");
        title.set_wrap(true);
        let list = gtk::ListBox::new();
        list.add_css_class("login-dialog-auth-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let list_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list_box.add_css_class("login-dialog-auth-list-view");
        list_box.append(&title);
        list_box.append(&list);
        list_box.set_visible(false);

        let button = gtk::Button::new();
        button.add_css_class("login-button");
        button.set_halign(gtk::Align::Center);
        button.set_visible(false);

        let web_title = gtk::Label::new(None);
        web_title.add_css_class("web-login-title-label");
        web_title.set_wrap(true);
        web_title.set_justify(gtk::Justification::Center);
        let qr = gtk::Picture::new();
        qr.set_can_shrink(false);
        qr.set_halign(gtk::Align::Center);
        let url = gtk::Label::new(None);
        url.add_css_class("web-login-url-label");
        url.set_selectable(true);
        let code_title = gtk::Label::new(Some("Login code: "));
        code_title.add_css_class("web-login-code-title-label");
        let code = gtk::Label::new(None);
        code.add_css_class("web-login-code-label");
        code.set_selectable(true);
        let code_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        code_box.set_halign(gtk::Align::Center);
        code_box.append(&code_title);
        code_box.append(&code);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("web-login-prompt-button");
        let done = gtk::Button::with_label("Done");
        done.add_css_class("web-login-prompt-button");
        done.add_css_class("default");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_homogeneous(true);
        buttons.append(&cancel);
        buttons.append(&done);
        let spinner = gtk::Spinner::new();
        spinner.add_css_class("web-login-spinner");
        spinner.set_visible(false);
        let web = gtk::Box::new(gtk::Orientation::Vertical, 12);
        web.add_css_class("web-login-prompt");
        web.append(&web_title);
        web.append(&qr);
        web.append(&url);
        web.append(&code_box);
        web.append(&buttons);
        web.append(&spinner);
        web.set_visible(false);

        let timed = gtk::ProgressBar::new();
        timed.add_css_class("login-dialog-timed-login-indicator");
        timed.update_property(&[gtk::accessible::Property::Label("Automatic login")]);
        timed.set_visible(false);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("login-dialog-input-well");
        root.append(&list_box);
        root.append(&button);
        root.append(&web);
        root.append(&timed);
        Rc::new(Self {
            root,
            list_box,
            title,
            list,
            button,
            web,
            web_title,
            qr,
            url,
            code_box,
            code,
            cancel,
            done,
            spinner,
            timed,
            shown: RefCell::default(),
            keys: RefCell::default(),
        })
    }

    /// `choose(answer)` for a list item, the button or Done;
    /// `cancel()` for web login's Cancel (authPrompt.js `_handleCancel`).
    pub fn connect(
        self: &Rc<Self>,
        choose: impl Fn(String) + 'static,
        cancel: impl Fn() + 'static,
    ) {
        let choose = Rc::new(choose);
        let weak = Rc::downgrade(self);
        let pick = choose.clone();
        self.list.connect_row_activated(move |_, row| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let key = usize::try_from(row.index())
                .ok()
                .and_then(|i| ui.keys.borrow().get(i).cloned());
            if let Some(key) = key {
                pick(key);
            }
        });
        let press = choose.clone();
        self.button.connect_clicked(move |_| press(String::new()));
        self.done.connect_clicked(move |_| choose(String::new()));
        self.cancel.connect_clicked(move |_| cancel());
    }

    /// Show the machine's step.
    pub fn render(&self, view: &View) {
        let changed = *self.shown.borrow() != view.ask;
        if changed {
            self.show(view.ask.as_ref());
        }
        let ask = view.ask.as_ref();
        self.list.set_sensitive(view.sensitive);
        self.button.set_sensitive(view.sensitive);
        self.done.set_sensitive(view.sensitive && !view.loading);
        self.web_title
            .set_opacity(if view.loading { 0.5 } else { 1.0 });
        self.qr.set_opacity(if view.loading { 0.5 } else { 1.0 });
        self.spinner.set_visible(view.loading);
        self.spinner.set_spinning(view.loading);
        if changed && view.sensitive {
            // `updateSensitivity` gives the shown widget the focus (once it
            // is mapped); the dialog's default button is Done.
            let target: Option<gtk::Widget> = match ask {
                Some(Ask::Choices { .. }) => self.list.row_at_index(0).map(|r| r.upcast()),
                Some(Ask::Button { .. }) => Some(self.button.clone().upcast()),
                Some(Ask::WebLogin { .. }) => Some(self.done.clone().upcast()),
                None => None,
            };
            if let Some(target) = target {
                glib::idle_add_local_once(move || {
                    target.grab_focus();
                });
            }
        }
        if view.loading && self.done.has_focus() {
            self.cancel.grab_focus();
        }
    }

    fn show(&self, ask: Option<&Ask>) {
        *self.shown.borrow_mut() = ask.cloned();
        self.list_box
            .set_visible(matches!(ask, Some(Ask::Choices { .. })));
        self.button
            .set_visible(matches!(ask, Some(Ask::Button { .. })));
        self.web
            .set_visible(matches!(ask, Some(Ask::WebLogin { .. })));
        match ask {
            Some(Ask::Choices { prompt, choices }) => {
                self.title.set_label(prompt);
                while let Some(row) = self.list.row_at_index(0) {
                    self.list.remove(&row);
                }
                for choice in choices {
                    let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    item.add_css_class("login-dialog-auth-list-item");
                    let title = gtk::Label::new(Some(&choice.title));
                    title.set_xalign(0.0);
                    title.add_css_class("login-dialog-auth-list-label");
                    item.append(&title);
                    if let Some(subtitle) = &choice.subtitle {
                        let label = gtk::Label::new(Some(subtitle));
                        label.set_xalign(0.0);
                        label.add_css_class("dim-label");
                        item.append(&label);
                    }
                    let row = gtk::ListBoxRow::new();
                    row.set_child(Some(&item));
                    row.update_property(&[gtk::accessible::Property::Label(&choice.title)]);
                    self.list.append(&row);
                }
                *self.keys.borrow_mut() = choices.iter().map(|c| c.key.clone()).collect();
                self.list.set_visible(!choices.is_empty());
            }
            Some(Ask::Button { label }) => self.button.set_label(label),
            Some(Ask::WebLogin { message, url, code }) => {
                self.web_title.set_label(message);
                self.qr.set_paintable(
                    qr_texture(url)
                        .as_ref()
                        .map(|t| t.upcast_ref::<gtk::gdk::Paintable>()),
                );
                self.qr
                    .update_property(&[gtk::accessible::Property::Label(url)]);
                let shown = display_url(url);
                if shown.len() > URL_LABEL_LONG_THRESHOLD {
                    self.url.add_css_class("web-login-url-label-long");
                } else {
                    self.url.remove_css_class("web-login-url-label-long");
                }
                self.url.set_label(&shown);
                self.code.set_label(code.as_deref().unwrap_or_default());
                self.code_box.set_visible(code.is_some());
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_show_without_scheme_or_trailing_slash() {
        assert_eq!(
            display_url("https://idp.example/device/"),
            "idp.example/device"
        );
        assert_eq!(display_url("http://a.example"), "a.example");
        assert_eq!(display_url("idp.example/x"), "idp.example/x");
    }

    #[test]
    fn qr_codes_are_whole_pixel_modules_with_a_quiet_zone() {
        let (pixels, side) = qr_pixels("https://idp.example/device").unwrap();
        assert!(side >= QR_CODE_SIZE);
        assert_eq!(pixels.len(), side * side * 3);
        // The quiet zone's corner is light; the finder pattern's is dark.
        assert_eq!(pixels[0], 255);
        let scale = side
            / (qrcode::QrCode::new(b"https://idp.example/device")
                .unwrap()
                .width()
                + 4);
        let finder = (2 * scale * side + 2 * scale) * 3;
        assert_eq!(pixels[finder], 0);
    }
}
