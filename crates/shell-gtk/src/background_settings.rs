//! Native color parsing and versioned wallpaper publication; no GTK initialization.

use gio::prelude::*;

/// Build strict placement/shading metadata without allowing an invalid
/// underlay color to suppress a valid wallpaper URI. GNOME Shell continues
/// loading its image after a failed pattern-color parse. Black is the
/// deterministic RGB underlay fallback; this does not make malformed CSS valid.
pub(super) fn picture_settings(
    placement: &str,
    shading: &str,
    primary: &str,
    secondary: &str,
) -> Option<tuna_shell_control::background::PictureSettings> {
    use tuna_shell_control::background::{PictureSettings, Placement, Shading};
    let color = |text: &str| {
        gtk4::gdk::RGBA::parse(text)
            .map(|rgb| {
                [rgb.red(), rgb.green(), rgb.blue()]
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            })
            .unwrap_or([0, 0, 0])
    };
    Some(PictureSettings {
        placement: Placement::from_key(placement)?,
        shading: Shading::from_key(shading)?,
        primary: color(primary),
        secondary: color(secondary),
    })
}

/// The complete publisher wire payload, shared with regression tests. Typed
/// metadata stays versioned; unknown placement/shading still fails upstream.
pub(super) fn append_metadata(
    mut text: String,
    lock_uri: &str,
    desktop: tuna_shell_control::background::PictureSettings,
    lock: tuna_shell_control::background::PictureSettings,
) -> Result<String, serde_json::Error> {
    text.push_str(lock_uri);
    text.push('\n');
    text.push_str(&serde_json::to_string(
        &tuna_shell_control::background::BackgroundMetadata {
            version: 1,
            desktop,
            lock,
        },
    )?);
    text.push('\n');
    Ok(text)
}

/// The lock screen's picture as a canonical escaped `file://` URI, or
/// empty: only absolute local files are published (the compositor
/// decodes once, so literal percent signs stay filename bytes).
fn lock_picture_uri(uri: &str) -> String {
    if !uri.starts_with("file://") {
        return String::new();
    }
    gio::File::for_uri(uri)
        .path()
        .filter(|path| path.is_absolute())
        .map(|path| gio::File::for_path(path).uri().to_string())
        .unwrap_or_default()
}

/// The compositor's wallpaper drop from GNOME's background settings
/// (the dark picture under the dark style, the accent, the lock
/// picture and both placements). `None` when a placement or shading is
/// unknown: the previous drop stays rather than a guessed one.
pub(super) fn drop_text(
    background: &gio::Settings,
    interface: Option<&gio::Settings>,
    screensaver: Option<&gio::Settings>,
) -> Option<String> {
    let has_key = |s: &gio::Settings, key: &str| {
        s.settings_schema()
            .is_some_and(|schema| schema.has_key(key))
    };
    let dark = interface.is_some_and(|i| i.string("color-scheme") == "prefer-dark");
    let text = crate::logic::wallpaper_drop(
        &background.string("picture-uri"),
        &background.string("picture-uri-dark"),
        dark,
        &background.string("picture-options"),
        &background.string("primary-color"),
        // GNOME 47's accent; older schemas have none (blue).
        &interface
            .filter(|i| has_key(i, "accent-color"))
            .map(|i| i.string("accent-color").to_string())
            .unwrap_or_default(),
    );
    let lock_uri = screensaver
        .filter(|s| has_key(s, "picture-uri"))
        .map(|s| lock_picture_uri(&s.string("picture-uri")))
        .unwrap_or_default();
    let placement = |s: &gio::Settings| {
        picture_settings(
            s.string("picture-options").as_str(),
            s.string("color-shading-type").as_str(),
            s.string("primary-color").as_str(),
            s.string("secondary-color").as_str(),
        )
    };
    let desktop = placement(background)?;
    let lock = match screensaver {
        Some(settings) => placement(settings)?,
        None => desktop,
    };
    append_metadata(text, &lock_uri, desktop, lock).ok()
}

/// Replace `dir/tuna-wallpaper` atomically (write, then rename): a
/// failed write leaves the compositor's previous wallpaper in place.
pub(super) fn publish_drop(dir: &std::path::Path, text: &str) -> std::io::Result<()> {
    let tmp = dir.join(".tuna-wallpaper.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, dir.join("tuna-wallpaper"))
}

#[cfg(test)]
mod background_metadata_tests {
    use super::{append_metadata, picture_settings};
    use crate::logic::wallpaper_drop;
    use tuna_shell_control::background::BackgroundMetadata;

    #[test]
    fn invalid_primary_or_secondary_never_suppresses_desktop_dark_or_lock_uri() {
        for (primary, secondary) in [("000000", "#ffffff"), ("#123456", "FFFFFF")] {
            let desktop = picture_settings("zoom", "solid", primary, secondary).unwrap();
            let lock = picture_settings("scaled", "horizontal", secondary, primary).unwrap();
            for dark in [false, true] {
                let text = append_metadata(
                    wallpaper_drop(
                        "file:///valid-light.jpg",
                        "file:///valid-dark.jpg",
                        dark,
                        "zoom",
                        primary,
                        "blue",
                    ),
                    "file:///valid-lock.jpg",
                    desktop,
                    lock,
                )
                .unwrap();
                let lines: Vec<_> = text.lines().collect();
                assert_eq!(lines.len(), 5);
                assert_eq!(
                    lines[0],
                    if dark {
                        "file:///valid-dark.jpg"
                    } else {
                        "file:///valid-light.jpg"
                    }
                );
                assert_eq!(lines[3], "file:///valid-lock.jpg");
                let metadata: BackgroundMetadata = serde_json::from_str(lines[4]).unwrap();
                assert_eq!(metadata.version, 1);
                assert_eq!(metadata.desktop, desktop);
                assert_eq!(metadata.lock, lock);
                if primary == "000000" {
                    assert_eq!(desktop.primary, [0, 0, 0]);
                }
                if secondary == "FFFFFF" {
                    assert_eq!(desktop.secondary, [0, 0, 0]);
                }
            }
        }
    }

    #[test]
    fn valid_color_channels_and_metadata_enums_remain_exact() {
        let actual =
            picture_settings("spanned", "vertical", "#123456", "rgb(200, 10, 20)").unwrap();
        assert_eq!(actual.primary, [0x12, 0x34, 0x56]);
        assert_eq!(actual.secondary, [200, 10, 20]);
        assert!(picture_settings("unknown", "solid", "000000", "FFFFFF").is_none());
        assert!(picture_settings("zoom", "unknown", "000000", "FFFFFF").is_none());
    }
}

#[cfg(test)]
mod wallpaper_settings_tests {
    use super::{drop_text, publish_drop};
    use crate::test_settings::{dispatch_until, SettingsFixture};
    use gio::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use tuna_shell_control::background::{BackgroundMetadata, Placement, Shading};

    const BACKGROUND_KEYS: &str = r#"
    <key name="picture-options" enum="org.gnome.desktop.GDesktopBackgroundStyle"><default>'zoom'</default></key>
    <key name="picture-uri" type="s"><default>'file:///usr/share/backgrounds/gnome/adwaita-l.jxl'</default></key>
    <key name="primary-color" type="s"><default>'#023c88'</default></key>
    <key name="secondary-color" type="s"><default>'#5789ca'</default></key>
    <key name="color-shading-type" enum="org.gnome.desktop.GDesktopBackgroundShading"><default>'solid'</default></key>"#;

    /// GNOME 51's background, interface and screensaver keys this
    /// binding reads (gsettings-desktop-schemas defaults), plus a
    /// pre-47 interface schema without accent-color.
    fn schemas() -> String {
        format!(
            r#"<schemalist>
  <enum id="org.gnome.desktop.GDesktopBackgroundStyle">
    <value nick="none" value="0"/><value nick="wallpaper" value="1"/>
    <value nick="centered" value="2"/><value nick="scaled" value="3"/>
    <value nick="stretched" value="4"/><value nick="zoom" value="5"/>
    <value nick="spanned" value="6"/>
  </enum>
  <enum id="org.gnome.desktop.GDesktopBackgroundShading">
    <value nick="solid" value="0"/><value nick="vertical" value="1"/>
    <value nick="horizontal" value="2"/>
  </enum>
  <enum id="org.gnome.desktop.GDesktopColorScheme">
    <value nick="default" value="0"/><value nick="prefer-dark" value="1"/>
    <value nick="prefer-light" value="2"/>
  </enum>
  <enum id="org.gnome.desktop.GDesktopAccentColor">
    <value nick="blue" value="0"/><value nick="teal" value="1"/>
    <value nick="green" value="2"/><value nick="yellow" value="3"/>
    <value nick="orange" value="4"/><value nick="red" value="5"/>
    <value nick="pink" value="6"/><value nick="purple" value="7"/>
    <value nick="slate" value="8"/>
  </enum>
  <schema id="org.gnome.desktop.background" path="/org/gnome/desktop/background/">{BACKGROUND_KEYS}
    <key name="picture-uri-dark" type="s"><default>'file:///usr/share/backgrounds/gnome/adwaita-d.jxl'</default></key>
  </schema>
  <schema id="org.gnome.desktop.screensaver" path="/org/gnome/desktop/screensaver/">{BACKGROUND_KEYS}
  </schema>
  <schema id="org.gnome.desktop.interface" path="/org/gnome/desktop/interface/">
    <key name="color-scheme" enum="org.gnome.desktop.GDesktopColorScheme"><default>'default'</default></key>
    <key name="accent-color" enum="org.gnome.desktop.GDesktopAccentColor"><default>'blue'</default></key>
  </schema>
  <schema id="org.gnome.desktop.interface.gnome46" path="/org/gnome/desktop/interface46/">
    <key name="color-scheme" enum="org.gnome.desktop.GDesktopColorScheme"><default>'default'</default></key>
  </schema>
</schemalist>"#
        )
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn metadata(text: &str) -> BackgroundMetadata {
        serde_json::from_str(text.lines().nth(4).unwrap()).unwrap()
    }

    #[test]
    fn drop_follows_gnome_background_settings() {
        let fixture = SettingsFixture::new(&schemas());
        let bg = fixture.settings("org.gnome.desktop.background");
        let ui = fixture.settings("org.gnome.desktop.interface");
        let saver = fixture.settings("org.gnome.desktop.screensaver");

        // GNOME's defaults: light picture, default accent, lock picture.
        let text = drop_text(&bg, Some(&ui), Some(&saver)).unwrap();
        assert_eq!(
            lines(&text)[..4],
            [
                "file:///usr/share/backgrounds/gnome/adwaita-l.jxl",
                "#023c88",
                "#3584e4",
                "file:///usr/share/backgrounds/gnome/adwaita-l.jxl",
            ]
        );
        assert_eq!(metadata(&text).desktop.placement, Placement::Zoom);

        // Dark style picks the dark picture; the accent follows.
        ui.set_string("color-scheme", "prefer-dark").unwrap();
        ui.set_string("accent-color", "purple").unwrap();
        let text = drop_text(&bg, Some(&ui), Some(&saver)).unwrap();
        assert_eq!(
            lines(&text)[0],
            "file:///usr/share/backgrounds/gnome/adwaita-d.jxl"
        );
        assert_eq!(lines(&text)[2], "#9141ac");

        // Desktop and lock placement/shading travel separately.
        bg.set_string("picture-options", "none").unwrap();
        bg.set_string("color-shading-type", "vertical").unwrap();
        saver.set_string("picture-options", "spanned").unwrap();
        let text = drop_text(&bg, Some(&ui), Some(&saver)).unwrap();
        assert_eq!(lines(&text)[0], "", "no picture: the color shows");
        let meta = metadata(&text);
        assert_eq!(
            (meta.desktop.placement, meta.desktop.shading),
            (Placement::None, Shading::Vertical)
        );
        assert_eq!(meta.lock.placement, Placement::Spanned);
        assert_eq!(meta.desktop.secondary, [0x57, 0x89, 0xca]);

        // Without a screensaver schema the lock reuses the desktop's.
        let meta = metadata(&drop_text(&bg, Some(&ui), None).unwrap());
        assert_eq!(meta.lock, meta.desktop);
        assert_eq!(lines(&drop_text(&bg, Some(&ui), None).unwrap())[3], "");

        // GSettings refuses values its enums do not know, so a typo in
        // dconf never reaches the compositor as a placement.
        assert!(bg.set_string("picture-options", "fill").is_err());
        assert_eq!(bg.string("picture-options"), "none");
    }

    #[test]
    fn older_interface_schemas_fall_back_to_blue() {
        let fixture = SettingsFixture::new(&schemas());
        let bg = fixture.settings("org.gnome.desktop.background");
        let old = fixture.settings("org.gnome.desktop.interface.gnome46");
        old.set_string("color-scheme", "prefer-dark").unwrap();
        let text = drop_text(&bg, Some(&old), None).unwrap();
        assert_eq!(
            lines(&text)[0],
            "file:///usr/share/backgrounds/gnome/adwaita-d.jxl"
        );
        assert_eq!(lines(&text)[2], "#3584e4");
        // No interface schema at all: light, blue.
        let text = drop_text(&bg, None, None).unwrap();
        assert_eq!(
            lines(&text)[0],
            "file:///usr/share/backgrounds/gnome/adwaita-l.jxl"
        );
        assert_eq!(lines(&text)[2], "#3584e4");
    }

    #[test]
    fn only_absolute_local_lock_pictures_are_published() {
        let fixture = SettingsFixture::new(&schemas());
        let bg = fixture.settings("org.gnome.desktop.background");
        let saver = fixture.settings("org.gnome.desktop.screensaver");
        let lock_line = |uri: &str| {
            saver.set_string("picture-uri", uri).unwrap();
            lines(&drop_text(&bg, None, Some(&saver)).unwrap())[3].clone()
        };
        // Escapes are normalised once; a literal percent stays a byte.
        assert_eq!(
            lock_line("file:///pics/a%20b.png"),
            "file:///pics/a%20b.png"
        );
        assert_eq!(
            lock_line("file:///pics/100%25.png"),
            "file:///pics/100%25.png"
        );
        for uri in [
            "",
            "/pics/a.png",
            "https://example.com/a.png",
            "resource:///org/gnome/a.png",
            "file:relative.png",
        ] {
            assert_eq!(lock_line(uri), "", "{uri:?}");
        }
        // A newline in the key cannot add lines to the drop.
        saver
            .set_string("picture-uri", "file:///pics/a\nfile:///etc/shadow")
            .unwrap();
        let text = drop_text(&bg, None, Some(&saver)).unwrap();
        assert_eq!(text.lines().count(), 5);
        assert!(!lines(&text)[3].contains("/etc/shadow\n"));
        assert!(serde_json::from_str::<BackgroundMetadata>(text.lines().nth(4).unwrap()).is_ok());
        // A rejected lock picture never suppresses the desktop's.
        assert_eq!(
            lines(&drop_text(&bg, None, Some(&saver)).unwrap())[0],
            "file:///usr/share/backgrounds/gnome/adwaita-l.jxl"
        );
    }

    #[test]
    fn settings_changes_republish_the_drop_and_failed_writes_keep_the_last() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let fixture = SettingsFixture::new(&schemas());
                let bg = fixture.settings("org.gnome.desktop.background");
                let runtime = tempfile::tempdir().unwrap();
                let drop = runtime.path().join("tuna-wallpaper");
                let published = Rc::new(Cell::new(0));
                // main.rs's binding: republish on every change.
                let (dir, count, settings) =
                    (runtime.path().to_owned(), published.clone(), bg.clone());
                bg.connect_changed(None, move |_, _| {
                    let text = drop_text(&settings, None, None).unwrap();
                    publish_drop(&dir, &text).unwrap();
                    count.set(count.get() + 1);
                });
                bg.set_string("picture-uri", "file:///pics/new.png")
                    .unwrap();
                dispatch_until(&context, || published.get() > 0);
                let text = std::fs::read_to_string(&drop).unwrap();
                assert_eq!(lines(&text)[0], "file:///pics/new.png");
                assert!(!runtime.path().join(".tuna-wallpaper.tmp").exists());

                // A runtime dir that cannot be written (gone, or full):
                // an error, no panic, and the previous drop stays whole.
                let gone = runtime.path().join("missing");
                assert!(publish_drop(&gone, "file:///pics/other.png\n").is_err());
                assert_eq!(std::fs::read_to_string(&drop).unwrap(), text);
            })
            .unwrap();
    }
}
