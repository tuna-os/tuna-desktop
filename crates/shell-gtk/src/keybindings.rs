//! GNOME Shell's own keybindings (`org.gnome.shell.keybindings`), as
//! windowManager.js, screenshot.js and brightnessManager.js bind them:
//! the overview and app view, the message tray and quick settings, the
//! dash's apps by number, screenshots, and the brightness keys. Keys
//! come from GSettings when GNOME's schema is installed, else GNOME 51's
//! defaults; the compositor grabs them like any accelerator.

use gtk4::gio;
use gtk4::prelude::*;

use tuna_shell_control::{
    SwitcherKey, SwitcherKeyKind, KEYSYM_ABOVE_TAB, MODE_LOCK_SCREEN, MODE_NORMAL, MODE_OVERVIEW,
    MODE_POPUP, MODE_UNLOCK_SCREEN,
};

pub const SCHEMA: &str = "org.gnome.shell.keybindings";
/// GNOME's window-manager keys (gsettings-desktop-schemas).
pub const WM_SCHEMA: &str = "org.gnome.desktop.wm.keybindings";
/// Mutter's own keys (half tiling).
pub const MUTTER_SCHEMA: &str = "org.gnome.mutter.keybindings";
pub const MEDIA_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
/// gnome-settings-daemon's media-keys plugin, which owns [`MEDIA_SCHEMA`]'s
/// bindings in GNOME.
pub const MEDIA_KEYS_DAEMON: &str = "org.gnome.SettingsDaemon.MediaKeys";

/// What a binding does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleOverview,
    LockScreen,
    /// GNOME's Run a Command dialog (`panel-run-dialog`, Alt+F2;
    /// main.js binds it in NORMAL|OVERVIEW).
    RunDialog,
    ToggleApplicationView,
    ToggleMessageTray,
    ToggleQuickSettings,
    /// Focus the nth dash app (1-based), launching it when not running.
    SwitchToApplication(u8),
    /// Open a new window of the nth dash app.
    OpenNewWindow(u8),
    ShowScreenshotUi,
    /// The screenshot UI in screencast mode, or the recording stopped.
    ShowScreenRecordingUi,
    Screenshot,
    ScreenshotWindow,
    VolumeUp {
        precise: bool,
    },
    VolumeDown {
        precise: bool,
    },
    VolumeMute,
    VolumeUpQuiet,
    VolumeDownQuiet,
    VolumeMuteQuiet,
    MicrophoneMute,
    BrightnessUp,
    BrightnessDown,
    BrightnessUpMonitor,
    BrightnessDownMonitor,
    BrightnessCycle,
    BrightnessCycleMonitor,
    FocusActiveNotification,
    ShiftOverviewUp,
    ShiftOverviewDown,
    /// `gsd-media-keys`' `magnifier`: flip `screen-magnifier-enabled`.
    ToggleMagnifier,
    /// `magnifier-zoom-in` / `magnifier-zoom-out`: one rounded step.
    MagnifierZoom {
        zoom_in: bool,
    },
    /// `gsd-media-keys`' `screenreader`: flip `screen-reader-enabled`.
    ToggleScreenReader,
    /// GNOME's Ctrl+Alt+Tab "Switch system controls"
    /// (`switch-panels`): move keyboard focus into the top bar, where
    /// Tab/arrows walk every control and Enter activates it. GNOME
    /// 51 shows a group switcher first (ctrlAltTab.js); with one bar
    /// the chord focuses it directly: the first control, or the last
    /// when `backward` (`switch-panels-backward`). Tray icons, the
    /// accessibility menu and the screencast stop button have no
    /// other keyboard route.
    FocusPanel {
        backward: bool,
    },
    /// Window-manager keys on the focused window or the workspaces.
    WindowMenu,
    ToggleMaximized,
    Unmaximize,
    BeginMove,
    BeginResize,
    /// GNOME's maximize, minimize and close keys on the focused window.
    Maximize,
    Minimize,
    Close,
    /// Mutter's half tiling (toggles back).
    TileLeft,
    TileRight,
    /// The next or previous keyboard input source.
    NextInputSource,
    PreviousInputSource,
    /// Switch to a workspace: first, last, or one step left/right.
    Workspace(Target),
    /// Move the focused window there and follow it.
    MoveToWorkspace(Target),
}

/// A workspace a key goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    First,
    Last,
    Left,
    Right,
}

impl Target {
    /// The workspace id for this target, from `active` among `workspaces`
    /// with windows on `occupied`. GNOME's workspaces are dynamic: one
    /// empty workspace always follows the last occupied one, and that is
    /// where Last goes; Right past the end is the next new one.
    pub fn resolve(self, active: u32, workspaces: &[u32], occupied: &[u32]) -> Option<u32> {
        let first = workspaces.iter().copied().min().unwrap_or(0);
        let trailing = occupied.iter().copied().max().map_or(0, |m| m + 1);
        let last = workspaces
            .iter()
            .copied()
            .max()
            .unwrap_or(active)
            .max(trailing);
        match self {
            Target::First => Some(first),
            Target::Last => Some(last),
            Target::Left => (active > first).then(|| active - 1),
            Target::Right => Some(active + 1),
        }
    }
}

/// Which schema a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Schema {
    Shell,
    Wm,
    Mutter,
    Media,
}

/// One key, its action, GNOME 51's default accelerators, and its modes.
struct Spec {
    schema: Schema,
    key: String,
    action: Action,
    defaults: Vec<&'static str>,
    modes: u32,
}

const NORMAL_OVERVIEW: u32 = MODE_NORMAL | MODE_OVERVIEW;
const ALL: u32 = MODE_NORMAL | MODE_OVERVIEW | MODE_LOCK_SCREEN | MODE_UNLOCK_SCREEN | MODE_POPUP;

const DIGIT_DEFAULTS: [&str; 9] = [
    "<Super>1", "<Super>2", "<Super>3", "<Super>4", "<Super>5", "<Super>6", "<Super>7", "<Super>8",
    "<Super>9",
];
const NEW_WINDOW_DEFAULTS: [&str; 9] = [
    "<Super><Control>1",
    "<Super><Control>2",
    "<Super><Control>3",
    "<Super><Control>4",
    "<Super><Control>5",
    "<Super><Control>6",
    "<Super><Control>7",
    "<Super><Control>8",
    "<Super><Control>9",
];

fn specs() -> Vec<Spec> {
    let mut specs = vec![
        Spec {
            schema: Schema::Media,
            key: "screensaver".into(),
            action: Action::LockScreen,
            defaults: vec!["<Super>l"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-overview".into(),
            action: Action::ToggleOverview,
            defaults: vec![],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-application-view".into(),
            action: Action::ToggleApplicationView,
            defaults: vec!["<Super>a"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-message-tray".into(),
            action: Action::ToggleMessageTray,
            defaults: vec!["<Super>v", "<Super>m"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "toggle-quick-settings".into(),
            action: Action::ToggleQuickSettings,
            defaults: vec!["<Super>s"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "show-screenshot-ui".into(),
            action: Action::ShowScreenshotUi,
            defaults: vec!["Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "show-screen-recording-ui".into(),
            action: Action::ShowScreenRecordingUi,
            defaults: vec!["<Ctrl><Shift><Alt>R"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screenshot".into(),
            action: Action::Screenshot,
            defaults: vec!["<Shift>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screenshot-window".into(),
            action: Action::ScreenshotWindow,
            defaults: vec!["<Alt>Print"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Shell,
            key: "screen-brightness-up".into(),
            action: Action::BrightnessUp,
            defaults: vec!["XF86MonBrightnessUp"],
            modes: ALL,
        },
        Spec {
            schema: Schema::Shell,
            key: "screen-brightness-down".into(),
            action: Action::BrightnessDown,
            defaults: vec!["XF86MonBrightnessDown"],
            modes: ALL,
        },
    ];
    for (key, action, defaults, modes) in [
        (
            "focus-active-notification",
            Action::FocusActiveNotification,
            vec!["<Super>n"],
            NORMAL_OVERVIEW,
        ),
        (
            "shift-overview-up",
            Action::ShiftOverviewUp,
            vec!["<Super><Alt>Up"],
            NORMAL_OVERVIEW,
        ),
        (
            "shift-overview-down",
            Action::ShiftOverviewDown,
            vec!["<Super><Alt>Down"],
            NORMAL_OVERVIEW,
        ),
        (
            "screen-brightness-up-monitor",
            Action::BrightnessUpMonitor,
            vec!["<Shift>XF86MonBrightnessUp"],
            ALL,
        ),
        (
            "screen-brightness-down-monitor",
            Action::BrightnessDownMonitor,
            vec!["<Shift>XF86MonBrightnessDown"],
            ALL,
        ),
        (
            "screen-brightness-cycle",
            Action::BrightnessCycle,
            vec!["XF86MonBrightnessCycle"],
            ALL,
        ),
        (
            "screen-brightness-cycle-monitor",
            Action::BrightnessCycleMonitor,
            vec!["<Shift>XF86MonBrightnessCycle"],
            ALL,
        ),
    ] {
        specs.push(Spec {
            schema: Schema::Shell,
            key: key.into(),
            action,
            defaults,
            modes,
        });
    }
    // GNOME separates editable bindings from its hardware-key defaults.
    // Both families remain live, and an explicitly empty array disables it.
    let media = |key: &str, action, defaults: Vec<&'static str>| Spec {
        schema: Schema::Media,
        key: key.into(),
        action,
        defaults,
        modes: ALL,
    };
    for (key, action, defaults) in [
        (
            "volume-up",
            Action::VolumeUp { precise: false },
            vec!["XF86AudioRaiseVolume", "<Ctrl>XF86AudioRaiseVolume"],
        ),
        (
            "volume-down",
            Action::VolumeDown { precise: false },
            vec!["XF86AudioLowerVolume", "<Ctrl>XF86AudioLowerVolume"],
        ),
        ("volume-mute", Action::VolumeMute, vec!["XF86AudioMute"]),
        ("mic-mute", Action::MicrophoneMute, vec!["XF86AudioMicMute"]),
        (
            "volume-up-quiet",
            Action::VolumeUpQuiet,
            vec![
                "<Alt>XF86AudioRaiseVolume",
                "<Alt><Ctrl>XF86AudioRaiseVolume",
            ],
        ),
        (
            "volume-down-quiet",
            Action::VolumeDownQuiet,
            vec![
                "<Alt>XF86AudioLowerVolume",
                "<Alt><Ctrl>XF86AudioLowerVolume",
            ],
        ),
        (
            "volume-mute-quiet",
            Action::VolumeMuteQuiet,
            vec!["<Alt>XF86AudioMute"],
        ),
        (
            "volume-up-precise",
            Action::VolumeUp { precise: true },
            vec![
                "<Shift>XF86AudioRaiseVolume",
                "<Ctrl><Shift>XF86AudioRaiseVolume",
            ],
        ),
        (
            "volume-down-precise",
            Action::VolumeDown { precise: true },
            vec![
                "<Shift>XF86AudioLowerVolume",
                "<Ctrl><Shift>XF86AudioLowerVolume",
            ],
        ),
    ] {
        specs.push(media(key, action, vec![]));
        specs.push(media(&format!("{key}-static"), action, defaults));
    }
    // The magnifier and screen reader keys (`shortcuts-list.h`:
    // SHELL_ACTION_MODE_ALL), GNOME 51 defaults in
    // `org.gnome.settings-daemon.plugins.media-keys`.
    specs.extend([
        media("magnifier", Action::ToggleMagnifier, vec!["<Alt><Super>8"]),
        media(
            "screenreader",
            Action::ToggleScreenReader,
            vec!["<Alt><Super>s"],
        ),
        media(
            "magnifier-zoom-in",
            Action::MagnifierZoom { zoom_in: true },
            vec!["<Alt><Super>equal"],
        ),
        media(
            "magnifier-zoom-out",
            Action::MagnifierZoom { zoom_in: false },
            vec!["<Alt><Super>minus"],
        ),
    ]);
    // GNOME's window-manager keys (Super+PageUp/PageDown and Alt+Tab
    // stay the compositor's own; it drops its defaults for the rest
    // once these are grabbed, so they rebind).
    let wm = |key: &str, action, defaults: Vec<&'static str>| Spec {
        schema: Schema::Wm,
        key: key.into(),
        action,
        defaults,
        modes: NORMAL_OVERVIEW,
    };
    specs.extend([
        wm(
            "activate-window-menu",
            Action::WindowMenu,
            vec!["<Alt>space"],
        ),
        wm(
            "toggle-maximized",
            Action::ToggleMaximized,
            vec!["<Alt>F10"],
        ),
        wm(
            "unmaximize",
            Action::Unmaximize,
            vec!["<Super>Down", "<Alt>F5"],
        ),
        wm("maximize", Action::Maximize, vec!["<Super>Up"]),
        wm("minimize", Action::Minimize, vec!["<Super>h"]),
        wm("close", Action::Close, vec!["<Alt>F4"]),
        Spec {
            schema: Schema::Wm,
            key: "switch-input-source".into(),
            action: Action::NextInputSource,
            defaults: vec!["<Super>space", "XF86Keyboard"],
            modes: ALL,
        },
        Spec {
            schema: Schema::Wm,
            key: "switch-input-source-backward".into(),
            action: Action::PreviousInputSource,
            defaults: vec!["<Shift><Super>space", "<Shift>XF86Keyboard"],
            modes: ALL,
        },
        Spec {
            schema: Schema::Mutter,
            key: "toggle-tiled-left".into(),
            action: Action::TileLeft,
            defaults: vec!["<Super>Left"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Mutter,
            key: "toggle-tiled-right".into(),
            action: Action::TileRight,
            defaults: vec!["<Super>Right"],
            modes: NORMAL_OVERVIEW,
        },
        wm("begin-move", Action::BeginMove, vec!["<Alt>F7"]),
        wm("panel-run-dialog", Action::RunDialog, vec!["<Alt>F2"]),
        wm("begin-resize", Action::BeginResize, vec!["<Alt>F8"]),
        // GNOME binds these in NORMAL|OVERVIEW|LOCK|UNLOCK|LOGIN
        // (windowManager.js `_startA11ySwitcher`); the lock screen
        // shows no panel here, so NORMAL|OVERVIEW.
        Spec {
            schema: Schema::Wm,
            key: "switch-panels".into(),
            action: Action::FocusPanel { backward: false },
            defaults: vec!["<Control><Alt>Tab"],
            modes: NORMAL_OVERVIEW,
        },
        Spec {
            schema: Schema::Wm,
            key: "switch-panels-backward".into(),
            action: Action::FocusPanel { backward: true },
            defaults: vec!["<Shift><Control><Alt>Tab"],
            modes: NORMAL_OVERVIEW,
        },
        wm(
            "switch-to-workspace-1",
            Action::Workspace(Target::First),
            vec!["<Super>Home"],
        ),
        wm(
            "switch-to-workspace-last",
            Action::Workspace(Target::Last),
            vec!["<Super>End"],
        ),
        wm(
            "switch-to-workspace-left",
            Action::Workspace(Target::Left),
            vec!["<Super>Page_Up", "<Super><Alt>Left", "<Control><Alt>Left"],
        ),
        wm(
            "switch-to-workspace-right",
            Action::Workspace(Target::Right),
            vec![
                "<Super>Page_Down",
                "<Super><Alt>Right",
                "<Control><Alt>Right",
            ],
        ),
        wm(
            "move-to-workspace-1",
            Action::MoveToWorkspace(Target::First),
            vec!["<Super><Shift>Home"],
        ),
        wm(
            "move-to-workspace-last",
            Action::MoveToWorkspace(Target::Last),
            vec!["<Super><Shift>End"],
        ),
        wm(
            "move-to-workspace-left",
            Action::MoveToWorkspace(Target::Left),
            vec![
                "<Super><Shift>Page_Up",
                "<Super><Shift><Alt>Left",
                "<Control><Shift><Alt>Left",
            ],
        ),
        wm(
            "move-to-workspace-right",
            Action::MoveToWorkspace(Target::Right),
            vec![
                "<Super><Shift>Page_Down",
                "<Super><Shift><Alt>Right",
                "<Control><Shift><Alt>Right",
            ],
        ),
    ]);
    for n in 1..=9u8 {
        specs.push(Spec {
            schema: Schema::Shell,
            key: format!("switch-to-application-{n}"),
            action: Action::SwitchToApplication(n),
            defaults: vec![DIGIT_DEFAULTS[usize::from(n - 1)]],
            modes: NORMAL_OVERVIEW,
        });
        specs.push(Spec {
            schema: Schema::Shell,
            key: format!("open-new-window-application-{n}"),
            action: Action::OpenNewWindow(n),
            defaults: vec![NEW_WINDOW_DEFAULTS[usize::from(n - 1)]],
            modes: NORMAL_OVERVIEW,
        });
    }
    specs
}

/// A bound accelerator: its action, accelerator string, modes and
/// GNOME grab flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub accelerator: String,
    pub modes: u32,
    pub flags: u32,
}

impl Action {
    /// The `MetaKeyBindingFlags` GNOME grabs this action with: the
    /// magnifier and screen reader toggles ignore autorepeat
    /// (`shortcuts-list.h` at gsd 51.0), so holding the chord flips once.
    pub fn grab_flags(self) -> u32 {
        match self {
            Action::ToggleMagnifier | Action::ToggleScreenReader => {
                tuna_shell_control::GRAB_IGNORE_AUTOREPEAT
            }
            _ => 0,
        }
    }
}

/// The current bindings: each key's accelerators from its GNOME schema
/// (`shell`, `wm` or `mutter`) when installed, else GNOME 51's defaults.
/// The media-keys schema's keys (volume, mic-mute, screensaver, magnifier) are
/// gnome-settings-daemon's in GNOME: while `gsd_media_keys` runs it grabs
/// them through org.gnome.Shell, so the shell binds none of them and the
/// grabs cannot collide. Without it (the nested preview, which starts no
/// session services) the shell keeps its native subset.
pub fn bindings(
    shell: Option<&gio::Settings>,
    wm: Option<&gio::Settings>,
    mutter: Option<&gio::Settings>,
    media: Option<&gio::Settings>,
    gsd_media_keys: bool,
) -> Vec<Binding> {
    specs()
        .into_iter()
        .filter(|spec| !(gsd_media_keys && spec.schema == Schema::Media))
        .flat_map(|spec| {
            let settings = match spec.schema {
                Schema::Shell => shell,
                Schema::Wm => wm,
                Schema::Mutter => mutter,
                Schema::Media => media,
            };
            let has = settings
                .and_then(|s| s.settings_schema())
                .is_some_and(|schema| schema.has_key(&spec.key));
            let accels: Vec<String> = match settings {
                Some(s) if has => s.strv(&spec.key).iter().map(|a| a.to_string()).collect(),
                _ => spec.defaults.iter().map(|a| (*a).to_owned()).collect(),
            };
            accels
                .into_iter()
                .filter(|a| !a.is_empty())
                .map(move |accelerator| Binding {
                    action: spec.action,
                    accelerator,
                    modes: spec.modes,
                    flags: spec.action.grab_flags(),
                })
        })
        .collect()
}

/// Whether a fired binding runs while Ctrl+Alt+Tab holds the bar's
/// exclusive keyboard (`mode`: the `MODE_*` bit it fired in). Session
/// chords pause while held: a NORMAL/OVERVIEW fire then is spurious
/// (the hold owns the keyboard), so only the hold's own chord —
/// re-anchoring the bar — and the lock screen stay live. POPUP-mode
/// system keys (volume, brightness) keep working: the hold lets them
/// through, as the compositor's popup mode does.
pub fn runs_while_panel_held(action: Action, mode: u32) -> bool {
    if mode != MODE_NORMAL && mode != MODE_OVERVIEW {
        return true;
    }
    matches!(action, Action::FocusPanel { .. } | Action::LockScreen)
}

/// The switcher's keys (`switch-applications`, `switch-group` and
/// their backward keys) from the window-manager schema when installed,
/// else GNOME 51's defaults. The compositor drives the switcher from
/// them: it holds the popup open while the chord's modifiers are held.
pub fn switcher_keys(wm: Option<&gio::Settings>) -> Vec<(String, SwitcherKeyKind)> {
    use SwitcherKeyKind as K;
    let mut keys: Vec<(&str, K, Vec<&str>)> = vec![
        (
            "switch-applications",
            K::Applications,
            vec!["<Super>Tab", "<Alt>Tab"],
        ),
        (
            "switch-applications-backward",
            K::ApplicationsBackward,
            vec!["<Shift><Super>Tab", "<Shift><Alt>Tab"],
        ),
        (
            "switch-group",
            K::Group,
            vec!["<Super>Above_Tab", "<Alt>Above_Tab"],
        ),
        (
            "switch-group-backward",
            K::GroupBackward,
            vec!["<Shift><Super>Above_Tab", "<Shift><Alt>Above_Tab"],
        ),
    ];
    keys.extend([
        ("switch-windows", K::Windows, vec![]),
        ("switch-windows-backward", K::WindowsBackward, vec![]),
        ("cycle-windows", K::CycleWindows, vec!["<Alt>Escape"]),
        (
            "cycle-windows-backward",
            K::CycleWindowsBackward,
            vec!["<Shift><Alt>Escape"],
        ),
        ("cycle-group", K::CycleGroup, vec!["<Alt>F6"]),
        (
            "cycle-group-backward",
            K::CycleGroupBackward,
            vec!["<Shift><Alt>F6"],
        ),
    ]);
    keys.into_iter()
        .flat_map(|(key, kind, defaults)| {
            let has = wm
                .and_then(|s| s.settings_schema())
                .is_some_and(|schema| schema.has_key(key));
            let accels: Vec<String> = match wm {
                Some(s) if has => s.strv(key).iter().map(|a| a.to_string()).collect(),
                _ => defaults.iter().map(|a| (*a).to_owned()).collect(),
            };
            accels
                .into_iter()
                .filter(|a| !a.is_empty())
                .map(move |a| (a, kind))
        })
        .collect()
}

/// A switcher accelerator as the compositor matches it: Mutter's
/// `Above_Tab` becomes [`KEYSYM_ABOVE_TAB`], the key above Tab.
pub fn parse_switcher_key(accelerator: &str, kind: SwitcherKeyKind) -> Option<SwitcherKey> {
    let (accelerator, above_tab) = match accelerator.strip_suffix("Above_Tab") {
        Some(mods) => (format!("{mods}Tab"), true),
        None => (accelerator.to_owned(), false),
    };
    let (keysym, mods) = crate::shell_dbus::parse_accelerator(&accelerator)?;
    Some(SwitcherKey {
        keysym: if above_tab { KEYSYM_ABOVE_TAB } else { keysym },
        mods,
        kind,
    })
}

/// GNOME Shell's keybinding schema, when installed.
pub fn settings() -> Option<gio::Settings> {
    schema(SCHEMA)
}

/// GNOME's window-manager keybinding schema, when installed.
pub fn wm_settings() -> Option<gio::Settings> {
    schema(WM_SCHEMA)
}

/// Mutter's keybinding schema, when installed.
pub fn media_settings() -> Option<gio::Settings> {
    schema(MEDIA_SCHEMA)
}

pub fn mutter_settings() -> Option<gio::Settings> {
    schema(MUTTER_SCHEMA)
}

fn schema(id: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(id, true)?;
    Some(gio::Settings::new(id))
}

/// GNOME's configurable percentage step (precise bindings always use 2%).
pub fn volume_step(media: Option<&gio::Settings>) -> f64 {
    media
        .filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key("volume-step"))
        })
        .map_or(6.0, |s| f64::from(s.int("volume-step").clamp(1, 20)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_targets_resolve_like_mutter() {
        let ws = [0, 1, 2];
        assert_eq!(Target::First.resolve(1, &ws, &[0, 1]), Some(0));
        assert_eq!(Target::Last.resolve(0, &ws, &[0, 1]), Some(2));
        assert_eq!(Target::Left.resolve(0, &ws, &[0]), None);
        assert_eq!(Target::Left.resolve(2, &ws, &[0]), Some(1));
        assert_eq!(Target::Right.resolve(2, &ws, &[0]), Some(3));
        // Everything on the first workspace: Last is the empty one after.
        assert_eq!(Target::Last.resolve(0, &[0], &[0]), Some(1));
    }

    #[test]
    fn switcher_keys_default_and_parse_like_mutter() {
        use tuna_shell_control::{MOD_ALT, MOD_LOGO, MOD_SHIFT};
        let keys: Vec<SwitcherKey> = switcher_keys(None)
            .iter()
            .filter_map(|(a, kind)| parse_switcher_key(a, *kind))
            .collect();
        assert_eq!(keys.len(), 12);
        let tab = 0xff09;
        assert_eq!(
            keys[1],
            SwitcherKey {
                keysym: tab,
                mods: MOD_ALT,
                kind: SwitcherKeyKind::Applications
            }
        );
        assert_eq!(
            keys[2],
            SwitcherKey {
                keysym: tab,
                mods: MOD_SHIFT | MOD_LOGO,
                kind: SwitcherKeyKind::ApplicationsBackward
            }
        );
        assert_eq!(
            keys[4],
            SwitcherKey {
                keysym: KEYSYM_ABOVE_TAB,
                mods: MOD_LOGO,
                kind: SwitcherKeyKind::Group
            }
        );
        assert_eq!(
            parse_switcher_key("<Control>grave", SwitcherKeyKind::Group).map(|k| k.keysym),
            Some(0x60)
        );
    }

    #[test]
    fn gnome_51_defaults_without_the_schema() {
        let all = bindings(None, None, None, None, false);
        let find = |action| {
            all.iter()
                .filter(|b| b.action == action)
                .map(|b| b.accelerator.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(find(Action::LockScreen), ["<Super>l"]);
        assert_eq!(
            find(Action::VolumeUp { precise: false }),
            ["XF86AudioRaiseVolume", "<Ctrl>XF86AudioRaiseVolume",]
        );
        assert_eq!(find(Action::VolumeMute), ["XF86AudioMute"]);
        assert_eq!(find(Action::ToggleMagnifier), ["<Alt><Super>8"]);
        assert_eq!(find(Action::ToggleScreenReader), ["<Alt><Super>s"]);
        assert_eq!(
            find(Action::MagnifierZoom { zoom_in: true }),
            ["<Alt><Super>equal"]
        );
        assert_eq!(
            find(Action::MagnifierZoom { zoom_in: false }),
            ["<Alt><Super>minus"]
        );
        let toggles =
            |action| matches!(action, Action::ToggleMagnifier | Action::ToggleScreenReader);
        for binding in all
            .iter()
            .filter(|b| toggles(b.action) || matches!(b.action, Action::MagnifierZoom { .. }))
        {
            // SHELL_ACTION_MODE_ALL: they work on the lock screen too.
            assert_eq!(binding.modes, ALL);
            assert_eq!(
                binding.flags & tuna_shell_control::GRAB_IGNORE_AUTOREPEAT != 0,
                toggles(binding.action)
            );
        }
        assert!(all
            .iter()
            .filter(|b| !toggles(b.action))
            .all(|b| b.flags == 0));
        assert_eq!(find(Action::VolumeMuteQuiet), ["<Alt>XF86AudioMute"]);
        assert_eq!(
            find(Action::VolumeUpQuiet),
            [
                "<Alt>XF86AudioRaiseVolume",
                "<Alt><Ctrl>XF86AudioRaiseVolume"
            ]
        );
        assert_eq!(
            find(Action::VolumeDownQuiet),
            [
                "<Alt>XF86AudioLowerVolume",
                "<Alt><Ctrl>XF86AudioLowerVolume"
            ]
        );
        assert_eq!(find(Action::MicrophoneMute), ["XF86AudioMicMute"]);
        assert_eq!(volume_step(None), 6.0);
        assert_eq!(find(Action::ToggleApplicationView), ["<Super>a"]);
        assert_eq!(find(Action::ToggleMessageTray), ["<Super>v", "<Super>m"]);
        assert_eq!(find(Action::ToggleQuickSettings), ["<Super>s"]);
        assert!(find(Action::ToggleOverview).is_empty());
        assert_eq!(find(Action::SwitchToApplication(3)), ["<Super>3"]);
        assert_eq!(find(Action::OpenNewWindow(9)), ["<Super><Control>9"]);
        assert_eq!(find(Action::BrightnessUp), ["XF86MonBrightnessUp"]);
        assert_eq!(find(Action::ShowScreenshotUi), ["Print"]);
        assert_eq!(find(Action::ShowScreenRecordingUi), ["<Ctrl><Shift><Alt>R"]);
        assert_eq!(find(Action::WindowMenu), ["<Alt>space"]);
        assert_eq!(find(Action::RunDialog), ["<Alt>F2"]);
        assert_eq!(
            find(Action::FocusPanel { backward: false }),
            ["<Control><Alt>Tab"]
        );
        assert_eq!(
            find(Action::FocusPanel { backward: true }),
            ["<Shift><Control><Alt>Tab"]
        );
        // GNOME 51's defaults for the keys the compositor used to own.
        assert_eq!(find(Action::Maximize), ["<Super>Up"]);
        assert_eq!(find(Action::Unmaximize), ["<Super>Down", "<Alt>F5"]);
        assert_eq!(find(Action::Minimize), ["<Super>h"]);
        assert_eq!(find(Action::BeginMove), ["<Alt>F7"]);
        assert_eq!(find(Action::BeginResize), ["<Alt>F8"]);
        assert_eq!(find(Action::Close), ["<Alt>F4"]);
        assert_eq!(find(Action::TileLeft), ["<Super>Left"]);
        assert_eq!(find(Action::TileRight), ["<Super>Right"]);
        assert_eq!(
            find(Action::NextInputSource),
            ["<Super>space", "XF86Keyboard"]
        );
        assert_eq!(
            find(Action::PreviousInputSource),
            ["<Shift><Super>space", "<Shift>XF86Keyboard"]
        );
        assert_eq!(find(Action::Workspace(Target::Last)), ["<Super>End"]);
        assert_eq!(
            find(Action::MoveToWorkspace(Target::Right)),
            [
                "<Super><Shift>Page_Down",
                "<Super><Shift><Alt>Right",
                "<Control><Shift><Alt>Right"
            ]
        );
        assert_eq!(find(Action::Workspace(Target::Left))[0], "<Super>Page_Up");
        let brightness = all
            .iter()
            .find(|b| b.action == Action::BrightnessUp)
            .unwrap();
        assert_ne!(
            brightness.modes & MODE_LOCK_SCREEN,
            0,
            "brightness keys work locked"
        );
        // Every default parses into a grabbable key.
        for b in &all {
            assert!(
                crate::shell_dbus::parse_accelerator(&b.accelerator).is_some(),
                "{}",
                b.accelerator
            );
        }
    }

    #[test]
    fn gsd_media_keys_owns_every_media_schema_key_and_nothing_else() {
        let native = bindings(None, None, None, None, false);
        let standing_down = bindings(None, None, None, None, true);
        let media = [
            Action::LockScreen,
            Action::VolumeUp { precise: false },
            Action::VolumeUp { precise: true },
            Action::VolumeDown { precise: false },
            Action::VolumeDown { precise: true },
            Action::VolumeMute,
            Action::VolumeUpQuiet,
            Action::VolumeDownQuiet,
            Action::VolumeMuteQuiet,
            Action::MicrophoneMute,
            Action::ToggleMagnifier,
            Action::ToggleScreenReader,
            Action::MagnifierZoom { zoom_in: true },
            Action::MagnifierZoom { zoom_in: false },
        ];
        for action in media {
            assert!(native.iter().any(|b| b.action == action), "{action:?}");
            assert!(
                !standing_down.iter().any(|b| b.action == action),
                "{action:?} still bound beside gsd-media-keys"
            );
        }
        // GNOME Shell's own keys stay the shell's: brightness lives in
        // org.gnome.shell.keybindings in GNOME 51, not in gsd.
        let rest: Vec<_> = native
            .iter()
            .filter(|b| !media.contains(&b.action))
            .cloned()
            .collect();
        assert_eq!(standing_down, rest);
        assert!(standing_down
            .iter()
            .any(|b| b.action == Action::BrightnessUp && b.accelerator == "XF86MonBrightnessUp"));
        // Nothing left in the shell's reserved set grabs a gsd key.
        for b in &standing_down {
            assert!(!b.accelerator.contains("XF86Audio"), "{b:?}");
            assert_ne!(b.accelerator, "<Super>l");
        }
    }

    #[test]
    fn panel_hold_pauses_session_chords_but_keeps_its_own_and_lock() {
        use tuna_shell_control::{MODE_LOCK_SCREEN, MODE_UNLOCK_SCREEN};
        // The G-PANEL-FOCUS regression: Super+S (NORMAL) must not run
        // while the bar holds the keyboard, in either session mode.
        for mode in [MODE_NORMAL, MODE_OVERVIEW] {
            assert!(!runs_while_panel_held(Action::ToggleQuickSettings, mode));
            assert!(!runs_while_panel_held(Action::ToggleMessageTray, mode));
            assert!(!runs_while_panel_held(Action::ToggleOverview, mode));
            assert!(!runs_while_panel_held(Action::SwitchToApplication(1), mode));
            assert!(!runs_while_panel_held(Action::WindowMenu, mode));
            // The hold's own chord re-anchors; the lock screen stays live.
            assert!(runs_while_panel_held(
                Action::FocusPanel { backward: false },
                mode
            ));
            assert!(runs_while_panel_held(
                Action::FocusPanel { backward: true },
                mode
            ));
            assert!(runs_while_panel_held(Action::LockScreen, mode));
        }
        // Popup-mode system keys and lock-screen fires pass through.
        for mode in [MODE_POPUP, MODE_LOCK_SCREEN, MODE_UNLOCK_SCREEN] {
            assert!(runs_while_panel_held(Action::ToggleQuickSettings, mode));
            assert!(runs_while_panel_held(Action::VolumeUpQuiet, mode));
            assert!(runs_while_panel_held(Action::LockScreen, mode));
        }
    }
}
