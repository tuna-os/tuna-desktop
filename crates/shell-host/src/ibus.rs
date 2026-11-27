//! Shared IBus wire values and bus discovery for the input bridge and GTK panel.

use gio::glib;
use gio::prelude::*;

/// The string inside a serialized IBusText (`v` holding `(sa{sv}sv)`).
pub fn text(value: &glib::Variant) -> Option<String> {
    // as_variant calls g_variant_get_variant directly; its Option result does
    // not protect ordinary serialized tuples from GLib's type assertion.
    let mut inner = value.clone();
    while inner.is_type(glib::VariantTy::VARIANT) {
        inner = inner.as_variant()?;
    }
    inner.try_child_value(2)?.str().map(str::to_owned)
}

/// A serialized IBusText with no attributes, as libibus sends one:
/// `v` holding `("IBusText", a{sv} {}, s text, v ("IBusAttrList",
/// a{sv} {}, av []))`.
pub fn text_variant(text: &str) -> glib::Variant {
    let no_props = || glib::VariantDict::new(None).end();
    let attrs = glib::Variant::tuple_from_iter([
        "IBusAttrList".to_variant(),
        no_props(),
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::VARIANT,
            std::iter::empty::<glib::Variant>(),
        ),
    ]);
    glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
        "IBusText".to_variant(),
        no_props(),
        text.to_variant(),
        glib::Variant::from_variant(&attrs),
    ]))
}

/// `IBUS_ADDRESS`, else what `ibus address` reports for this display.
pub fn address() -> Option<String> {
    if let Some(address) = std::env::var("IBUS_ADDRESS").ok().filter(|a| !a.is_empty()) {
        return Some(address);
    }
    let out = std::process::Command::new("ibus")
        .arg("address")
        .output()
        .ok()?;
    let address = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!address.is_empty() && address != "(null)").then_some(address)
}

/// Enable the existing opt-in IBus diagnostics in either process.
pub fn debug() -> bool {
    std::env::var_os("TUNA_IBUS_DEBUG").is_some()
}

/// The environment variable that names the systemd user unit owning the
/// session's `ibus-daemon`. The compositor sets it for the hardware
/// session only, where its session manager starts the GNOME session units
/// (`org.freedesktop.IBus.session.GNOME.service` is `WantedBy=
/// gnome-session.target`). Unset, nothing but the bridge starts a daemon.
pub const SESSION_UNIT_ENV: &str = "TUNA_IBUS_UNIT";

/// The systemd user unit that owns the session's `ibus-daemon`, if the
/// compositor named one.
pub fn session_unit() -> Option<String> {
    std::env::var(SESSION_UNIT_ENV)
        .ok()
        .filter(|unit| !unit.is_empty())
}

/// What the user manager says about the session's IBus unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// There is no such unit (or it is masked): nothing else will start
    /// a daemon.
    Absent,
    /// The manager did not answer. The unit may exist, so nothing is
    /// started on a guess.
    Unknown,
    Loaded {
        activity: Activity,
        /// `gnome-session-initialized.target` is active, the unit's
        /// `Requisite=`: a start job would be accepted now.
        session_initialized: bool,
    },
}

/// A unit's `ActiveState`, as far as the decision cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// active, activating, reloading or refreshing: the manager owns a
    /// daemon, or is bringing one up.
    Up,
    /// inactive or failed: a start job may begin it.
    Down,
    /// deactivating, maintenance or anything unrecognised.
    Busy,
}

/// What the bridge does while no IBus bus answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Daemon {
    /// No unit exists: the bridge supervises its own `ibus-daemon` child.
    SpawnChild,
    /// The unit is down although its session is up: ask the manager to
    /// start it (and nothing else).
    StartUnit,
    /// The manager owns the daemon, or is about to: look again later.
    Wait,
}

/// Exactly one party may own the daemon. With a unit, that party is the
/// systemd user manager: it starts the daemon with the session, keeps it
/// in the unit's cgroup and stops it with the session. The bridge never
/// starts a second one, not even while the unit is merely late, and only
/// asks the manager to start the unit (once: `asked`) when the session is
/// up and nothing has. Only with no unit does the bridge own a daemon.
pub fn daemon_plan(unit: Unit, asked: bool) -> Daemon {
    match unit {
        Unit::Absent => Daemon::SpawnChild,
        Unit::Unknown => Daemon::Wait,
        Unit::Loaded {
            activity: Activity::Down,
            session_initialized: true,
        } if !asked => Daemon::StartUnit,
        Unit::Loaded { .. } => Daemon::Wait,
    }
}

/// `gnome-session-initialized.target`: the unit's `Requisite=`.
pub const SESSION_INITIALIZED_TARGET: &str = "gnome-session-initialized.target";

/// The unit's state from `systemctl show -p LoadState -p ActiveState`
/// (`Key=Value` lines, in the manager's order). `session_initialized` is
/// asked only when the unit is down, the one state that needs it.
pub fn parse_unit(show: &str, session_initialized: impl FnOnce() -> bool) -> Unit {
    let property = |name: &str| {
        show.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };
    let (Some(load), Some(active)) = (property("LoadState"), property("ActiveState")) else {
        return Unit::Unknown;
    };
    if load != "loaded" {
        return Unit::Absent;
    }
    let activity = match active {
        "active" | "activating" | "reloading" | "refreshing" => Activity::Up,
        "inactive" | "failed" => Activity::Down,
        _ => Activity::Busy,
    };
    Unit::Loaded {
        activity,
        session_initialized: activity == Activity::Down && session_initialized(),
    }
}

/// Whether the bridge replaces an `ibus-daemon` it owns that has exited:
/// the unit's `Restart=on-abnormal`, so a daemon a signal ended comes back
/// and one that quit (`ibus exit`, or a failed start) stays gone.
pub fn restart_owned(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibus_text_encoding_matches_the_libibus_wire_shape() {
        let packed = text_variant("你好");
        assert_eq!(packed.type_().as_str(), "v");
        let inner = packed.as_variant().unwrap();
        assert_eq!(inner.type_().as_str(), "(sa{sv}sv)");
        assert_eq!(inner.child_value(0).str(), Some("IBusText"));
        let attrs = inner.child_value(3).as_variant().unwrap();
        assert_eq!(attrs.type_().as_str(), "(sa{sv}av)");
        assert_eq!(attrs.child_value(0).str(), Some("IBusAttrList"));
        assert_eq!(attrs.child_value(2).n_children(), 0);
        assert_eq!(text(&packed).as_deref(), Some("你好"));
    }

    #[test]
    fn raw_and_nested_ibus_text_decode_without_native_type_assertions() {
        let boxed = text_variant("你好");
        for value in [
            boxed.as_variant().unwrap(),
            boxed.clone(),
            glib::Variant::from_variant(&boxed),
        ] {
            assert_eq!(text(&value).as_deref(), Some("你好"));
        }
        for value in ["not a container".to_variant(), 42u32.to_variant()] {
            assert_eq!(text(&value), None);
        }
    }

    fn loaded(activity: Activity, session_initialized: bool) -> Unit {
        Unit::Loaded {
            activity,
            session_initialized,
        }
    }

    #[test]
    fn only_a_session_without_an_ibus_unit_gets_a_bridge_child() {
        assert_eq!(daemon_plan(Unit::Absent, false), Daemon::SpawnChild);
        // Every state of an existing unit leaves the daemon to the manager.
        for activity in [Activity::Up, Activity::Down, Activity::Busy] {
            for initialized in [false, true] {
                for asked in [false, true] {
                    assert_ne!(
                        daemon_plan(loaded(activity, initialized), asked),
                        Daemon::SpawnChild,
                        "{activity:?} initialized={initialized} asked={asked}"
                    );
                }
            }
        }
        // A manager that does not answer may still own a daemon.
        assert_eq!(daemon_plan(Unit::Unknown, false), Daemon::Wait);
    }

    #[test]
    fn a_late_unit_is_waited_for_never_raced() {
        // The bridge runs ~50 ms before the session manager starts the
        // unit (#1014): before the session is up, a down unit is awaited.
        assert_eq!(
            daemon_plan(loaded(Activity::Down, false), false),
            Daemon::Wait
        );
        assert_eq!(
            daemon_plan(loaded(Activity::Up, false), false),
            Daemon::Wait
        );
        assert_eq!(daemon_plan(loaded(Activity::Up, true), false), Daemon::Wait);
        assert_eq!(
            daemon_plan(loaded(Activity::Busy, true), false),
            Daemon::Wait
        );
    }

    #[test]
    fn a_down_unit_in_a_running_session_is_started_through_the_manager_once() {
        assert_eq!(
            daemon_plan(loaded(Activity::Down, true), false),
            Daemon::StartUnit
        );
        assert_eq!(
            daemon_plan(loaded(Activity::Down, true), true),
            Daemon::Wait
        );
    }

    #[test]
    fn unit_properties_parse_to_the_state_the_plan_uses() {
        let show = |load: &str, active: &str| format!("LoadState={load}\nActiveState={active}\n");
        for (active, expected) in [
            ("active", Activity::Up),
            ("activating", Activity::Up),
            ("reloading", Activity::Up),
            ("inactive", Activity::Down),
            ("failed", Activity::Down),
            ("deactivating", Activity::Busy),
            ("maintenance", Activity::Busy),
        ] {
            assert_eq!(
                parse_unit(&show("loaded", active), || true),
                loaded(expected, expected == Activity::Down),
                "{active}"
            );
        }
        // Not shipped, or masked: nothing else will start a daemon.
        assert_eq!(
            parse_unit(&show("not-found", "inactive"), || true),
            Unit::Absent
        );
        assert_eq!(
            parse_unit(&show("masked", "inactive"), || true),
            Unit::Absent
        );
        // Property order is the manager's, not the request's.
        assert_eq!(
            parse_unit("ActiveState=active\nLoadState=loaded\n", || true),
            loaded(Activity::Up, false)
        );
        // Unreadable output may hide an existing unit.
        assert_eq!(parse_unit("", || true), Unit::Unknown);
        assert_eq!(parse_unit("LoadState=loaded\n", || true), Unit::Unknown);
    }

    #[test]
    fn the_session_target_is_only_asked_about_for_a_down_unit() {
        let show = "LoadState=loaded\nActiveState=active\n";
        parse_unit(show, || panic!("an active unit needs no target check"));
        let down = "LoadState=loaded\nActiveState=failed\n";
        assert_eq!(parse_unit(down, || false), loaded(Activity::Down, false));
    }

    #[test]
    fn an_owned_daemon_comes_back_only_from_an_abnormal_exit() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;
        // wait(2) statuses: killed by SIGKILL (9), by SIGSEGV (11) with a
        // core dump (0x80).
        assert!(restart_owned(ExitStatus::from_raw(9)));
        assert!(restart_owned(ExitStatus::from_raw(11 | 0x80)));
        // Exited 0 (`ibus exit`) or 1 (another daemon owns the display).
        assert!(!restart_owned(ExitStatus::from_raw(0)));
        assert!(!restart_owned(ExitStatus::from_raw(1 << 8)));
    }
}
