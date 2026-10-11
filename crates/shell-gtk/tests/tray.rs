//! Tray protocol model across registration, icon, menu, and lifecycle
//! sequences. The shell is a binary crate with no library to link, so
//! the `#[path]` include compiles the real module instead — the same
//! shape as the scoped proof drivers in `examples/`.
// The include also compiles the panel widgets; tests touch only the model.
#![allow(dead_code)]

#[path = "../src/tray.rs"]
mod tray;

use tray::model::*;
use tuna_shell_host::watcher::{MenuEntry, FALLBACK_ITEM_PATH, WATCHER_NAME, WATCHER_PATH};

fn row(id: i32, label: &str, enabled: bool) -> MenuEntry {
    MenuEntry {
        id,
        label: label.to_owned(),
        enabled,
    }
}

#[test]
fn protocol_constants_match_spec() {
    assert_eq!(WATCHER_NAME, "org.kde.StatusNotifierWatcher");
    assert_eq!(WATCHER_PATH, "/StatusNotifierWatcher");
    assert_eq!(FALLBACK_ITEM_PATH, "/StatusNotifierItem");
    assert_eq!(ITEM_INTERFACE, "org.kde.StatusNotifierItem");
    assert_eq!(MENU_INTERFACE, "com.canonical.dbusmenu");
    assert_eq!(NAME_OWNER_CHANGED, "NameOwnerChanged");
}

#[test]
fn service_strings_parse_table() {
    let table: &[(&str, Option<(&str, &str)>)] = &[
        (
            "org.test.App",
            Some(("org.test.App", "/StatusNotifierItem")),
        ),
        ("org.test.App/Item", Some(("org.test.App", "/Item"))),
        ("org.test.App/a/b", Some(("org.test.App", "/a/b"))),
        (":1.42/Menu", Some((":1.42", "/Menu"))),
        (
            "org.test.App/",
            Some(("org.test.App", "/StatusNotifierItem")),
        ),
        ("", None),
        ("/Item", None),
    ];
    for (input, expected) in table {
        assert_eq!(
            ServiceRef::parse(input),
            expected.map(|(bus, path)| ServiceRef {
                bus_name: bus.to_owned(),
                object_path: path.to_owned(),
            }),
            "service {input:?} must parse as {expected:?}"
        );
    }
}

#[test]
fn registry_lifecycle_register_update_owner_change_retract() {
    let mut host = TrayRegistry::new();
    for service in ["org.a/Item", "org.b", ":1.5/Menu"] {
        assert_eq!(host.register(service), RegisterOutcome::Added);
    }
    // A property snapshot lands for the first item.
    let mut first = TrayItem::new("org.a/Item").expect("test service parses");
    first.title = "A".to_owned();
    first.status = ItemStatus::NeedsAttention;
    first.attention = IconChannel::parse("a-alarm".to_owned(), Vec::new());
    first.menu_path = parse_menu_path("/Menu");
    host.upsert(first);
    // The unique name's owner exits: only its item vanishes.
    assert_eq!(
        host.apply_name_owner_changed(":1.5", ":1.5", ""),
        vec![":1.5/Menu".to_owned()]
    );
    assert!(host.unregister("org.b"));
    assert!(!host.unregister("org.b"));
    assert_eq!(host.items().len(), 1);
    let survivor = host.get("org.a/Item").expect("survivor stays hosted");
    assert_eq!(survivor.title, "A");
    assert_eq!(survivor.primary_action(), PrimaryAction::OpenMenu);
    assert_eq!(
        survivor.select_icon(|name| name == "a-alarm", |_| false),
        IconSource::ThemeName("a-alarm".to_owned())
    );
}

#[test]
fn menu_diff_sequence_reorder_add_remove_relabel() {
    let before = vec![
        row(1, "Open", true),
        row(2, "Save", true),
        row(3, "Quit", true),
    ];
    let after = vec![
        row(2, "Save", true),
        row(1, "Open", false),
        row(4, "About", true),
    ];
    assert_eq!(
        diff_menu(&before, &after),
        vec![
            MenuDiff::Removed { id: 3 },
            MenuDiff::Added { id: 4, index: 2 },
            MenuDiff::Moved {
                id: 2,
                from: 1,
                to: 0
            },
            MenuDiff::Moved {
                id: 1,
                from: 0,
                to: 1
            },
            MenuDiff::Changed { id: 1 },
        ]
    );
}

#[test]
fn icon_fallback_resolves_real_theme_dir() {
    let root = tempfile::tempdir().expect("temp theme dir");
    let icons = root.path().join("icons");
    let status_dir = icons.join("hicolor/24x24/status");
    std::fs::create_dir_all(&status_dir).expect("theme dirs");
    let art = status_dir.join("app.png");
    std::fs::write(&art, b"fake-png").expect("theme file");
    let lookup = ThemeLookup {
        theme: "Tuna".to_owned(),
        icon_dirs: vec![icons],
        pixmaps_dir: root.path().join("pixmaps"),
    };
    assert_eq!(
        find_icon_path("app", &lookup, |path| path.exists()),
        Some(art)
    );
    assert_eq!(
        find_icon_path("missing", &lookup, |path| path.exists()),
        None
    );

    // Selection through real probes: the theme name wins while present.
    let mut item = TrayItem::new("org.test.App").expect("test service parses");
    item.icon.name = "app".to_owned();
    let in_theme = |name: &str| find_icon_path(name, &lookup, |path| path.exists()).is_some();
    assert_eq!(
        item.select_icon(in_theme, |path| path.exists()),
        IconSource::ThemeName("app".to_owned())
    );
    // An unknown name with a pixmap backing falls through to the pixmap.
    item.icon.name = "missing".to_owned();
    item.icon.pixmaps = vec![Pixmap::from_wire(2, 2, vec![0u8; 16]).expect("valid")];
    assert!(matches!(
        item.select_icon(
            |name| find_icon_path(name, &lookup, |path| path.exists()).is_some(),
            |path| path.exists()
        ),
        IconSource::Pixmap(_)
    ));
}

#[test]
fn activation_descriptors_carry_bus_path_and_args() {
    let mut item = TrayItem::new("org.test.App/Item").expect("test service parses");
    assert_eq!(item.primary_action(), PrimaryAction::Activate);
    item.menu_path = parse_menu_path("/Menu");
    assert_eq!(item.primary_action(), PrimaryAction::OpenMenu);

    assert_eq!(
        item.activate_call(10, 20),
        MethodCall {
            bus_name: "org.test.App".to_owned(),
            object_path: "/Item".to_owned(),
            interface: ITEM_INTERFACE,
            member: "Activate",
            args: CallArgs::Position { x: 10, y: 20 },
        }
    );
    assert_eq!(
        item.xayatana_secondary_activate_call(4242),
        MethodCall {
            bus_name: "org.test.App".to_owned(),
            object_path: "/Item".to_owned(),
            interface: ITEM_INTERFACE,
            member: "XAyatanaSecondaryActivate",
            args: CallArgs::Timestamp { timestamp: 4242 },
        }
    );
    assert_eq!(
        item.menu_event_call(7, 4242),
        Some(MethodCall {
            bus_name: "org.test.App".to_owned(),
            object_path: "/Menu".to_owned(),
            interface: MENU_INTERFACE,
            member: "Event",
            args: CallArgs::MenuEvent {
                id: 7,
                timestamp: 4242
            },
        })
    );
}
