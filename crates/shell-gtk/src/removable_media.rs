//! Removable media: GNOME Shell 51's `automountManager.js` and
//! `autorunManager.js` session components (`js/ui/components/`, tag 51.0).
//!
//! Automount: a volume the GIO volume monitor adds is mounted when
//! `org.gnome.desktop.media-handling automount` is on, the volume asks to
//! be (`g_volume_should_automount`) and can be, the logind session is the
//! active one (`org.gnome.SessionManager SessionIsActive`) and nothing
//! holds an automount inhibitor (`IsInhibited(16)`, re-read on every
//! `InhibitorAdded`/`InhibitorRemoved`). Hotplugged volumes mount with the
//! shell's mount operation ([`crate::mount_operation`]), which asks for a
//! LUKS/VeraCrypt passphrase and asks again after a wrong one. GNOME runs
//! both components only in the `user` session mode (`sessionMode.js`
//! `USER_SESSION_COMPONENTS`): while the screen is locked nothing is
//! mounted or offered, and unlocking mounts the volumes that arrived
//! meanwhile (`_startupMountAll`) without autorun.
//!
//! Autorun: a new mount of a volume this shell mounted (or let through)
//! in the last ten seconds is matched against the `autorun-*` keys by its
//! first `x-content/*` type: start the type's default app, open the
//! folder, do nothing, or post the "“name” connected" notification
//! offering each default app ("Open with …"); its body opens the default
//! file manager. GNOME's notification has no Eject action, so neither has
//! this one. With no `x-content` type the mount is sniffed in-process
//! ([`crate::hotplug_sniffer`]) in place of gnome-shell's helper.
//!
//! The decisions are pure ([`Core`], [`plan`], …) over a volume key, so
//! the tests drive them with fake volumes; [`RemovableMedia`] feeds them
//! from `GVolumeMonitor` and carries them out.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

pub const SCHEMA: &str = "org.gnome.desktop.media-handling";
pub const KEY_AUTOMOUNT: &str = "automount";
pub const KEY_AUTORUN_NEVER: &str = "autorun-never";
pub const KEY_START_APP: &str = "autorun-x-content-start-app";
pub const KEY_IGNORE: &str = "autorun-x-content-ignore";
pub const KEY_OPEN_FOLDER: &str = "autorun-x-content-open-folder";

/// `GNOME_SESSION_AUTOMOUNT_INHIBIT` (gnome-session `GSM_INHIBITOR_FLAG_AUTOMOUNT`).
pub const AUTOMOUNT_INHIBIT: u32 = 16;
/// `AUTORUN_EXPIRE_TIMEOUT_SECS`: how long after its mount a volume may autorun.
pub const AUTORUN_EXPIRE: Duration = Duration::from_secs(10);
/// The type GNOME asks for the file manager by.
pub const DIRECTORY_TYPE: &str = "inode/directory";

/// `_checkAndMountVolume`'s params.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckParams {
    pub check_session: bool,
    pub use_mount_op: bool,
    pub allow_autorun: bool,
}

impl CheckParams {
    /// `volume-added`: the defaults.
    pub const HOTPLUG: Self = Self {
        check_session: true,
        use_mount_op: true,
        allow_autorun: true,
    };
    /// `_startupMountAll`, on enable (shell start or unlock).
    pub const STARTUP: Self = Self {
        check_session: false,
        use_mount_op: false,
        allow_autorun: false,
    };
}

/// What a volume reports (`get_mount`, `should_automount`, `can_mount`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VolumeFacts {
    pub mounted: bool,
    pub should_automount: bool,
    pub can_mount: bool,
}

/// Mount a volume, with or without the shell's mount operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MountRequest {
    pub use_mount_op: bool,
}

/// `volume.allowAutorun` with its pending ten-second expiries. Every
/// `_allowAutorunExpire` adds its own timer and none is ever replaced,
/// so the earliest pending one wins even over a later `_allowAutorun`.
#[derive(Debug, Default)]
struct Grant {
    allowed: bool,
    expiries: Vec<Instant>,
}

impl Grant {
    fn settle(&mut self, now: Instant) {
        let before = self.expiries.len();
        self.expiries.retain(|at| *at > now);
        if self.expiries.len() != before {
            self.allowed = false;
        }
    }
}

/// The two components' shared state: whether they are enabled (the
/// `user` session mode, i.e. unlocked), the automount inhibitor, and
/// each volume's autorun grant.
#[derive(Debug)]
pub struct Core<V> {
    enabled: bool,
    inhibited: bool,
    grants: HashMap<V, Grant>,
}

impl<V> Default for Core<V> {
    fn default() -> Self {
        Self {
            enabled: false,
            inhibited: false,
            grants: HashMap::new(),
        }
    }
}

impl<V: Eq + Hash + Clone> Core<V> {
    /// `enable()`: true when this turned the components on, so the
    /// caller schedules `_startupMountAll`.
    pub fn enable(&mut self) -> bool {
        !std::mem::replace(&mut self.enabled, true)
    }

    /// `disable()`: true when this turned them off.
    pub fn disable(&mut self) -> bool {
        std::mem::replace(&mut self.enabled, false)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The `IsInhibited(AUTOMOUNT)` answer.
    pub fn set_inhibited(&mut self, inhibited: bool) {
        self.inhibited = inhibited;
    }

    /// `_onVolumeAdded`: nothing while disabled (GNOME disconnects).
    pub fn volume_added(
        &mut self,
        volume: &V,
        facts: VolumeFacts,
        session_active: bool,
        automount: bool,
        now: Instant,
    ) -> Option<MountRequest> {
        if !self.enabled {
            return None;
        }
        self.check_volume(
            volume,
            facts,
            CheckParams::HOTPLUG,
            session_active,
            automount,
            now,
        )
    }

    /// `_checkAndMountVolume`, recording the autorun grant it gives.
    pub fn check_volume(
        &mut self,
        volume: &V,
        facts: VolumeFacts,
        params: CheckParams,
        session_active: bool,
        automount: bool,
        now: Instant,
    ) -> Option<MountRequest> {
        if params.check_session && !session_active {
            return None;
        }
        if self.inhibited || facts.mounted {
            return None;
        }
        if !automount || !facts.should_automount || !facts.can_mount {
            // A mount added later (blank optical media, a manual
            // mount) may still autorun for ten seconds.
            self.allow_autorun(volume);
            self.expire_autorun(volume, now);
            return None;
        }
        if params.allow_autorun {
            self.allow_autorun(volume);
        }
        Some(MountRequest {
            use_mount_op: params.use_mount_op,
        })
    }

    /// `_onVolumeMounted`: the grant expires ten seconds after each
    /// attempt ends, whether it mounted or failed.
    pub fn mount_finished(&mut self, volume: &V, now: Instant) {
        self.expire_autorun(volume, now);
    }

    /// `_onVolumeRemoved`.
    pub fn volume_removed(&mut self, volume: &V) {
        self.grants.remove(volume);
    }

    /// `shouldAutorunMount`: a mount without a volume, of a volume not
    /// allowed to autorun now, or under a hidden directory never does.
    pub fn should_autorun(&mut self, volume: Option<&V>, root_hidden: bool, now: Instant) -> bool {
        let Some(volume) = volume else {
            return false;
        };
        let allowed = self.grants.get_mut(volume).is_some_and(|grant| {
            grant.settle(now);
            grant.allowed
        });
        allowed && !root_hidden
    }

    fn allow_autorun(&mut self, volume: &V) {
        self.grants.entry(volume.clone()).or_default().allowed = true;
    }

    fn expire_autorun(&mut self, volume: &V, now: Instant) {
        let grant = self.grants.entry(volume.clone()).or_default();
        grant.settle(now);
        grant.expiries.push(now + AUTORUN_EXPIRE);
    }
}

/// `isMountRootHidden`, for native roots only (`shouldAutorunMount`).
pub fn is_mount_root_hidden(native: bool, path: Option<&str>) -> bool {
    native && path.is_some_and(|p| p.contains("/."))
}

/// `isMountNonLocal`: `volume` is `None` without a volume, else its
/// `class` identifier.
pub fn is_mount_non_local(volume: Option<Option<&str>>) -> bool {
    match volume {
        None => true,
        Some(class) => class == Some("network"),
    }
}

/// `AutorunSetting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutorunSetting {
    Run,
    Ignore,
    Files,
    Ask,
}

/// The `org.gnome.desktop.media-handling` values, read when used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub automount: bool,
    pub autorun_never: bool,
    pub start_app: Vec<String>,
    pub ignore: Vec<String>,
    pub open_folder: Vec<String>,
}

impl Default for Prefs {
    /// The schema's defaults.
    fn default() -> Self {
        Self {
            automount: true,
            autorun_never: false,
            start_app: Vec::new(),
            ignore: Vec::new(),
            open_folder: Vec::new(),
        }
    }
}

impl Prefs {
    /// `_getAutorunSettingForType`: start-app, then ignore, then open-folder.
    pub fn setting_for(&self, content_type: &str) -> AutorunSetting {
        let has = |list: &[String]| list.iter().any(|t| t == content_type);
        if has(&self.start_app) {
            AutorunSetting::Run
        } else if has(&self.ignore) {
            AutorunSetting::Ignore
        } else if has(&self.open_folder) {
            AutorunSetting::Files
        } else {
            AutorunSetting::Ask
        }
    }

    /// `guessContentTypes`: scan only with autorun on and a local mount.
    pub fn should_scan(&self, non_local: bool) -> bool {
        !self.autorun_never && !non_local
    }
}

/// `guessContentTypes`: Windows software is never offered.
pub fn filter_content_types(types: Vec<String>) -> Vec<String> {
    types
        .into_iter()
        .filter(|t| t != "x-content/win32-software")
        .collect()
}

/// What `AutorunDispatcher.addMount` does with a new mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutorunPlan {
    Nothing,
    /// Launch the default app for this type on the mount's root; post
    /// the notification instead when there is none or it fails.
    Launch(String),
    /// Post the notification.
    Ask,
}

/// `addMount`: the first content type's setting decides.
pub fn plan(prefs: &Prefs, should_autorun: bool, content_types: &[String]) -> AutorunPlan {
    if prefs.autorun_never || !should_autorun {
        return AutorunPlan::Nothing;
    }
    let Some(first) = content_types.first() else {
        return AutorunPlan::Ask;
    };
    match prefs.setting_for(first) {
        AutorunSetting::Ignore => AutorunPlan::Nothing,
        AutorunSetting::Run => AutorunPlan::Launch(first.clone()),
        AutorunSetting::Files => AutorunPlan::Launch(DIRECTORY_TYPE.to_owned()),
        AutorunSetting::Ask => AutorunPlan::Ask,
    }
}

/// `_onVolumeMounted`: the udisks/cryptsetup/libblockdev messages that
/// mean "wrong or missing passphrase, ask again".
pub fn needs_password_again(message: &str) -> bool {
    [
        "No key available with this passphrase",
        "No key available to unlock device",
        "Failed to activate device: Incorrect passphrase",
        "Failed to load device's parameters: Invalid argument",
        "Failed to load device's parameters: Operation not permitted",
    ]
    .iter()
    .any(|m| message.contains(m))
}

/// `_onVolumeMounted`: udisks without VeraCrypt PIM support.
pub fn pim_unsupported(message: &str) -> bool {
    message.contains(
        "Compiled against a version of libcryptsetup that does not support the VeraCrypt PIM setting",
    )
}

/// The notification `_addNotification` posts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: &'static str,
    /// `(action key, label)`, one per offered app, in order.
    pub actions: Vec<(String, String)>,
}

/// `_addNotification`'s text: one "Open with" action per app.
pub fn notice(mount_name: &str, app_names: &[String]) -> Notice {
    Notice {
        title: format!("“{mount_name}” connected"),
        body: "Disk can now be used",
        actions: app_names
            .iter()
            .enumerate()
            .map(|(i, name)| (format!("open-{i}"), format!("Open with {name}")))
            .collect(),
    }
}

pub use runtime::RemovableMedia;

mod runtime {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gio::prelude::*;
    use gtk4::{gio, glib};

    use crate::mount_operation::ShellMountOperation;
    use crate::notify::NotifyUi;
    use tuna_shell_host::notifications::Urgency;

    const SESSION_NAME: &str = "org.gnome.SessionManager";
    const SESSION_PATH: &str = "/org/gnome/SessionManager";
    /// GNOME's system notification source (`messageTray.js` `getSystemSource`).
    const SYSTEM_SOURCE: &str = "System";
    const SYSTEM_ICON: &str = "cog-wheel-symbolic";

    pub struct RemovableMedia {
        app: gtk4::Application,
        monitor: gio::VolumeMonitor,
        settings: gio::Settings,
        session: RefCell<Option<gio::DBusProxy>>,
        core: RefCell<Core<gio::Volume>>,
        cancellable: RefCell<gio::Cancellable>,
        startup: RefCell<Option<glib::SourceId>>,
        operations: RefCell<HashMap<gio::Volume, Option<Rc<ShellMountOperation>>>>,
        notices: RefCell<HashMap<gio::Mount, u64>>,
        notify: Rc<NotifyUi>,
        /// The `IsInhibited` answer last asked for (older replies lose).
        inhibit_query: Cell<u64>,
    }

    impl RemovableMedia {
        /// Start both components, disabled until [`Self::sync_lock`]
        /// learns the session is unlocked. `None` without the schema.
        pub fn start(app: &gtk4::Application, notify: Rc<NotifyUi>) -> Option<Rc<Self>> {
            let Some(settings) = crate::settings(SCHEMA) else {
                eprintln!("tuna-shell-gtk: removable media: no {SCHEMA} schema; automount off");
                return None;
            };
            let me = Rc::new(Self {
                app: app.clone(),
                monitor: gio::VolumeMonitor::get(),
                settings,
                session: RefCell::new(None),
                core: RefCell::new(Core::default()),
                cancellable: RefCell::new(gio::Cancellable::new()),
                startup: RefCell::new(None),
                operations: RefCell::new(HashMap::new()),
                notices: RefCell::new(HashMap::new()),
                notify,
                inhibit_query: Cell::new(0),
            });
            me.connect_session();
            me.connect_monitor();
            Some(me)
        }

        /// The session mode: components run only while unlocked. `None`
        /// (no authoritative lock state yet) changes nothing.
        pub fn sync_lock(self: &Rc<Self>, locked: Option<bool>) {
            match locked {
                Some(false) => self.enable(),
                Some(true) => self.disable(),
                None => {}
            }
        }

        fn enable(self: &Rc<Self>) {
            if !self.core.borrow_mut().enable() {
                return;
            }
            *self.cancellable.borrow_mut() = gio::Cancellable::new();
            let weak = Rc::downgrade(self);
            let id = glib::idle_add_local_once(move || {
                if let Some(me) = weak.upgrade() {
                    me.startup.borrow_mut().take();
                    me.startup_mount_all();
                }
            });
            *self.startup.borrow_mut() = Some(id);
        }

        fn disable(&self) {
            if !self.core.borrow_mut().disable() {
                return;
            }
            self.cancellable.borrow().cancel();
            if let Some(id) = self.startup.borrow_mut().take() {
                id.remove();
            }
        }

        fn prefs(&self) -> Prefs {
            let strv = |key| {
                self.settings
                    .strv(key)
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            };
            Prefs {
                automount: self.settings.boolean(KEY_AUTOMOUNT),
                autorun_never: self.settings.boolean(KEY_AUTORUN_NEVER),
                start_app: strv(KEY_START_APP),
                ignore: strv(KEY_IGNORE),
                open_folder: strv(KEY_OPEN_FOLDER),
            }
        }

        /// `this._session.SessionIsActive`: false while the session
        /// manager is unknown, as an unset proxy property is in GJS.
        fn session_active(&self) -> bool {
            self.session
                .borrow()
                .as_ref()
                .and_then(|p| p.cached_property("SessionIsActive"))
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false)
        }

        fn connect_session(self: &Rc<Self>) {
            let weak = Rc::downgrade(self);
            gio::DBusProxy::for_bus(
                gio::BusType::Session,
                gio::DBusProxyFlags::DO_NOT_AUTO_START,
                None,
                SESSION_NAME,
                SESSION_PATH,
                SESSION_NAME,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    let proxy = match res {
                        Ok(proxy) => proxy,
                        Err(e) => {
                            eprintln!("tuna-shell-gtk: removable media: session manager: {e}");
                            return;
                        }
                    };
                    let weak = Rc::downgrade(&me);
                    proxy.connect_local("g-signal", false, move |args| {
                        let signal = args.get(2).and_then(|v| v.get::<String>().ok());
                        if matches!(
                            signal.as_deref(),
                            Some("InhibitorAdded" | "InhibitorRemoved")
                        ) {
                            if let Some(me) = weak.upgrade() {
                                me.query_inhibited();
                            }
                        }
                        None
                    });
                    *me.session.borrow_mut() = Some(proxy);
                    // GNOME reads it only on a change; this shell can
                    // restart under a live session, so read it once now.
                    me.query_inhibited();
                },
            );
        }

        /// `_InhibitorsChanged`: `IsInhibited(AUTOMOUNT)`.
        fn query_inhibited(self: &Rc<Self>) {
            let Some(proxy) = self.session.borrow().clone() else {
                return;
            };
            let serial = self.inhibit_query.get() + 1;
            self.inhibit_query.set(serial);
            let weak = Rc::downgrade(self);
            proxy.call(
                "IsInhibited",
                Some(&(AUTOMOUNT_INHIBIT,).to_variant()),
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    if me.inhibit_query.get() != serial {
                        return;
                    }
                    if let Ok(Some((inhibited,))) = res.map(|v| v.get::<(bool,)>()) {
                        me.core.borrow_mut().set_inhibited(inhibited);
                    }
                },
            );
        }

        fn connect_monitor(self: &Rc<Self>) {
            let weak = Rc::downgrade(self);
            self.monitor.connect_volume_added(move |_, volume| {
                if let Some(me) = weak.upgrade() {
                    me.volume_added(volume);
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_volume_removed(move |_, volume| {
                if let Some(me) = weak.upgrade() {
                    me.core.borrow_mut().volume_removed(volume);
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_drive_connected(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.drive_sound("device-added", "External drive connected");
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_drive_disconnected(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.drive_sound("device-removed", "External drive disconnected");
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_drive_eject_button(move |_, drive| {
                if let Some(me) = weak.upgrade() {
                    me.drive_eject_button(drive);
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_mount_added(move |_, mount| {
                if let Some(me) = weak.upgrade() {
                    let mount = mount.clone();
                    glib::spawn_future_local(async move { me.mount_added(mount).await });
                }
            });
            let weak = Rc::downgrade(self);
            self.monitor.connect_mount_removed(move |_, mount| {
                if let Some(me) = weak.upgrade() {
                    me.mount_removed(mount);
                }
            });
        }

        fn facts(volume: &gio::Volume) -> VolumeFacts {
            VolumeFacts {
                mounted: volume.get_mount().is_some(),
                should_automount: volume.should_automount(),
                can_mount: volume.can_mount(),
            }
        }

        fn volume_added(self: &Rc<Self>, volume: &gio::Volume) {
            let request = self.core.borrow_mut().volume_added(
                volume,
                Self::facts(volume),
                self.session_active(),
                self.prefs().automount,
                Instant::now(),
            );
            if let Some(request) = request {
                let operation = request
                    .use_mount_op
                    .then(|| ShellMountOperation::new(&self.app, volume, None));
                self.mount_volume(volume, operation);
            }
        }

        fn startup_mount_all(self: &Rc<Self>) {
            let automount = self.prefs().automount;
            for volume in self.monitor.volumes() {
                let request = self.core.borrow_mut().check_volume(
                    &volume,
                    Self::facts(&volume),
                    CheckParams::STARTUP,
                    self.session_active(),
                    automount,
                    Instant::now(),
                );
                if request.is_some() {
                    self.mount_volume(&volume, None);
                }
            }
        }

        /// `_mountVolume` (the autorun grant is already recorded).
        fn mount_volume(
            self: &Rc<Self>,
            volume: &gio::Volume,
            operation: Option<Rc<ShellMountOperation>>,
        ) {
            eprintln!(
                "tuna-shell-gtk: removable media: mounting {:?}{}",
                volume.name(),
                if operation.is_some() {
                    " (mount operation)"
                } else {
                    ""
                }
            );
            let mount_op = operation.as_ref().map(|o| o.mount_op().clone());
            self.operations
                .borrow_mut()
                .insert(volume.clone(), operation);
            let weak = Rc::downgrade(self);
            let mounted = volume.clone();
            let cancellable = self.cancellable.borrow().clone();
            volume.mount(
                gio::MountMountFlags::NONE,
                mount_op.as_ref(),
                Some(&cancellable),
                move |res| {
                    if let Some(me) = weak.upgrade() {
                        me.volume_mounted(&mounted, res);
                    }
                },
            );
        }

        /// `_onVolumeMounted`.
        fn volume_mounted(self: &Rc<Self>, volume: &gio::Volume, res: Result<(), glib::Error>) {
            self.core
                .borrow_mut()
                .mount_finished(volume, Instant::now());
            let Err(e) = res else {
                eprintln!(
                    "tuna-shell-gtk: removable media: mounted {:?}",
                    volume.name()
                );
                self.close_operation(volume);
                return;
            };
            if needs_password_again(e.message()) {
                self.reask_password(volume);
                return;
            }
            if pim_unsupported(e.message()) {
                self.notify.post(
                    SYSTEM_SOURCE,
                    SYSTEM_ICON,
                    "Unable to unlock volume",
                    "The installed udisks version does not support the PIM setting",
                );
            }
            if !e.matches(gio::IOErrorEnum::FailedHandled) {
                eprintln!(
                    "tuna-shell-gtk: removable media: unable to mount volume {}: {e}",
                    volume.name()
                );
            }
            self.close_operation(volume);
        }

        /// `_reaskPassword`: a new operation keeping the open dialog.
        fn reask_password(self: &Rc<Self>, volume: &gio::Volume) {
            let previous = self.operations.borrow().get(volume).cloned().flatten();
            let existing = previous.and_then(|op| op.borrow_dialog());
            let operation = ShellMountOperation::new(&self.app, volume, existing);
            self.mount_volume(volume, Some(operation));
        }

        fn close_operation(&self, volume: &gio::Volume) {
            let operation = self.operations.borrow_mut().remove(volume).flatten();
            if let Some(operation) = operation {
                operation.close();
            }
        }

        /// `_onDriveConnected` / `_onDriveDisconnected`.
        fn drive_sound(&self, id: &str, description: &str) {
            if self.core.borrow().enabled() && self.session_active() {
                crate::sound_feedback::play_event(id, description);
            }
        }

        /// `_onDriveEjectButton`: forced stop, or eject.
        fn drive_eject_button(&self, drive: &gio::Drive) {
            if !self.core.borrow().enabled() || !self.session_active() {
                return;
            }
            if drive.can_stop() {
                drive.stop(
                    gio::MountUnmountFlags::FORCE,
                    None::<&gio::MountOperation>,
                    gio::Cancellable::NONE,
                    |res| {
                        if let Err(e) = res {
                            eprintln!("tuna-shell-gtk: removable media: unable to stop the drive after drive-eject-button {e}");
                        }
                    },
                );
            } else if drive.can_eject() {
                drive.eject_with_operation(
                    gio::MountUnmountFlags::FORCE,
                    None::<&gio::MountOperation>,
                    gio::Cancellable::NONE,
                    |res| {
                        if let Err(e) = res {
                            eprintln!("tuna-shell-gtk: removable media: unable to eject the drive after drive-eject-button {e}");
                        }
                    },
                );
            }
        }

        /// `AutorunManager._onMountAdded` then `guessContentTypes`.
        async fn mount_added(self: Rc<Self>, mount: gio::Mount) {
            if !self.core.borrow().enabled() || !self.session_active() {
                return;
            }
            let volume = mount.volume();
            let class = volume.as_ref().map(|v| v.identifier("class"));
            let non_local = is_mount_non_local(class.as_ref().map(|c| c.as_deref()));
            let mut types = Vec::new();
            if self.prefs().should_scan(non_local) {
                match mount.guess_content_type_future(false).await {
                    Ok(found) => types = found.iter().map(|t| t.to_string()).collect(),
                    Err(e) => eprintln!(
                        "tuna-shell-gtk: removable media: unable to guess content types on added mount {}: {e}",
                        mount.name()
                    ),
                }
                if types.is_empty() {
                    types = crate::hotplug_sniffer::sniff_uri(mount.root().uri().into()).await;
                }
            }
            let types = filter_content_types(types);
            let mut apps: Vec<gio::AppInfo> = types
                .iter()
                .filter_map(|t| gio::AppInfo::default_for_type(t, false))
                .collect();
            if apps.is_empty() {
                apps.extend(gio::AppInfo::default_for_type(DIRECTORY_TYPE, false));
            }
            eprintln!(
                "tuna-shell-gtk: removable media: mount {:?} content types {types:?}",
                mount.name()
            );
            self.add_mount(&mount, &apps, &types);
        }

        /// `AutorunDispatcher.addMount`.
        fn add_mount(self: &Rc<Self>, mount: &gio::Mount, apps: &[gio::AppInfo], types: &[String]) {
            let root = mount.root();
            let path = root.path();
            let hidden =
                is_mount_root_hidden(root.is_native(), path.as_ref().and_then(|p| p.to_str()));
            let allowed = self.core.borrow_mut().should_autorun(
                mount.volume().as_ref(),
                hidden,
                Instant::now(),
            );
            let plan = plan(&self.prefs(), allowed, types);
            eprintln!(
                "tuna-shell-gtk: removable media: autorun {:?}: {plan:?}",
                mount.name()
            );
            match plan {
                AutorunPlan::Nothing => {}
                AutorunPlan::Ask => self.add_notification(mount, apps),
                AutorunPlan::Launch(content_type) => {
                    let launched = gio::AppInfo::default_for_type(&content_type, false)
                        .is_some_and(|app| start_app_for_mount(&app, mount));
                    // Also when the default app or file manager failed.
                    if !launched {
                        self.add_notification(mount, apps);
                    }
                }
            }
        }

        /// `_addNotification`: one per mount.
        fn add_notification(self: &Rc<Self>, mount: &gio::Mount, apps: &[gio::AppInfo]) {
            if self.notices.borrow().contains_key(mount) {
                return;
            }
            let names: Vec<String> = apps.iter().map(|a| a.name().to_string()).collect();
            let notice = notice(&mount.name(), &names);
            let mut actions = notice.actions.clone();
            // Activating the notification opens the file manager.
            actions.push(("default".to_owned(), String::new()));
            let Some(id) = self.notify.post_with_actions(
                SYSTEM_SOURCE,
                SYSTEM_ICON,
                &notice.title,
                notice.body,
                &actions,
                Urgency::Normal,
                None,
            ) else {
                return;
            };
            let handler = |app: Option<gio::AppInfo>| -> Rc<dyn Fn()> {
                let weak = Rc::downgrade(self);
                let mount = mount.clone();
                Rc::new(move || {
                    if let Some(app) = app.as_ref() {
                        start_app_for_mount(app, &mount);
                    }
                    // Activated notifications are destroyed.
                    if let Some(me) = weak.upgrade() {
                        me.notices.borrow_mut().remove(&mount);
                    }
                })
            };
            for ((key, _), app) in notice.actions.iter().zip(apps) {
                self.notify
                    .on_notification_action(id, key, handler(Some(app.clone())));
            }
            let files = gio::AppInfo::default_for_type(DIRECTORY_TYPE, false);
            self.notify
                .on_notification_action(id, "default", handler(files));
            self.notices.borrow_mut().insert(mount.clone(), id);
        }

        /// `removeMount`.
        fn mount_removed(&self, mount: &gio::Mount) {
            if let Some(id) = self.notices.borrow_mut().remove(mount) {
                self.notify.remove_notification(id);
            }
        }
    }

    /// `startAppForMount`: launch `app` on the mount's root.
    fn start_app_for_mount(app: &gio::AppInfo, mount: &gio::Mount) -> bool {
        let context = tuna_shell_host::apps::application_launch_context();
        match app.launch(&[mount.root()], Some(&context)) {
            Ok(()) => {
                eprintln!(
                    "tuna-shell-gtk: removable media: launched {} on {}",
                    app.name(),
                    mount.root().uri()
                );
                true
            }
            Err(e) => {
                eprintln!(
                    "tuna-shell-gtk: removable media: unable to launch the app {}: {e}",
                    app.name()
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USB: VolumeFacts = VolumeFacts {
        mounted: false,
        should_automount: true,
        can_mount: true,
    };

    fn types(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn unlocked() -> Core<u32> {
        let mut core = Core::default();
        assert!(core.enable());
        core
    }

    #[test]
    fn automount_mounts_a_hotplugged_volume_with_the_mount_operation() {
        let now = Instant::now();
        let mut core = unlocked();
        assert_eq!(
            core.volume_added(&1, USB, true, true, now),
            Some(MountRequest { use_mount_op: true })
        );
        // Autorun is allowed from the mount request until ten seconds
        // after the mount finishes.
        assert!(core.should_autorun(Some(&1), false, now));
        core.mount_finished(&1, now + Duration::from_secs(3));
        assert!(core.should_autorun(Some(&1), false, now + Duration::from_secs(12)));
        assert!(!core.should_autorun(Some(&1), false, now + Duration::from_secs(13)));
    }

    #[test]
    fn automount_refuses_inactive_inhibited_mounted_and_locked() {
        let now = Instant::now();
        let mut core = unlocked();
        // Another seat's session is in front: nothing happens at all.
        assert_eq!(core.volume_added(&1, USB, false, true, now), None);
        assert!(!core.should_autorun(Some(&1), false, now));
        // An automount inhibitor (e.g. an installer) blocks it.
        core.set_inhibited(true);
        assert_eq!(core.volume_added(&1, USB, true, true, now), None);
        core.set_inhibited(false);
        // Already mounted: left alone.
        let mounted = VolumeFacts {
            mounted: true,
            ..USB
        };
        assert_eq!(core.volume_added(&1, mounted, true, true, now), None);
        // Locked (the unlock-dialog mode has no automount component).
        assert!(core.disable());
        assert_eq!(core.volume_added(&2, USB, true, true, now), None);
        assert!(!core.should_autorun(Some(&2), false, now));
        assert!(!core.disable());
    }

    #[test]
    fn automount_off_or_unwanted_volumes_only_allow_autorun_for_ten_seconds() {
        let now = Instant::now();
        let mut core = unlocked();
        for (facts, automount) in [
            (USB, false),
            (
                VolumeFacts {
                    should_automount: false,
                    ..USB
                },
                true,
            ),
            (
                VolumeFacts {
                    can_mount: false,
                    ..USB
                },
                true,
            ),
        ] {
            core.volume_removed(&7);
            assert_eq!(core.volume_added(&7, facts, true, automount, now), None);
            // A mount added by hand (or a blank disc) still autoruns...
            assert!(core.should_autorun(Some(&7), false, now + Duration::from_secs(9)));
            // ...but not after the ten-second window.
            assert!(!core.should_autorun(Some(&7), false, now + AUTORUN_EXPIRE));
        }
    }

    #[test]
    fn unlock_mounts_waiting_volumes_without_autorun() {
        let now = Instant::now();
        let mut core: Core<u32> = Core::default();
        // Plugged while locked: ignored.
        assert_eq!(core.volume_added(&3, USB, true, true, now), None);
        // Unlock: enable() schedules _startupMountAll, which skips the
        // session check and the mount operation and grants no autorun.
        assert!(core.enable());
        assert!(!core.enable());
        assert_eq!(
            core.check_volume(&3, USB, CheckParams::STARTUP, false, true, now),
            Some(MountRequest {
                use_mount_op: false
            })
        );
        core.mount_finished(&3, now);
        assert!(!core.should_autorun(Some(&3), false, now));
        // An inhibitor still holds at startup.
        core.set_inhibited(true);
        assert_eq!(
            core.check_volume(&4, USB, CheckParams::STARTUP, true, true, now),
            None
        );
    }

    #[test]
    fn autorun_grant_keeps_the_earliest_expiry_and_dies_with_the_volume() {
        let now = Instant::now();
        let mut core = unlocked();
        core.volume_added(&1, USB, true, true, now);
        // A wrong passphrase: the attempt ends (expiry armed at +10 s)
        // and the re-ask mounts again without re-granting.
        core.mount_finished(&1, now);
        core.mount_finished(&1, now + Duration::from_secs(8));
        assert!(core.should_autorun(Some(&1), false, now + Duration::from_secs(9)));
        assert!(!core.should_autorun(Some(&1), false, now + Duration::from_secs(10)));
        // Removal forgets the grant.
        core.volume_added(&1, USB, true, true, now);
        core.volume_removed(&1);
        assert!(!core.should_autorun(Some(&1), false, now));
        // A mount without a volume never autoruns.
        assert!(!core.should_autorun(None, false, now));
    }

    #[test]
    fn hidden_native_roots_and_network_mounts() {
        let now = Instant::now();
        let mut core = unlocked();
        core.volume_added(&1, USB, true, true, now);
        assert!(!core.should_autorun(Some(&1), true, now));
        assert!(is_mount_root_hidden(true, Some("/home/u/.cache/m")));
        assert!(!is_mount_root_hidden(true, Some("/run/media/u/STICK")));
        // Only native roots are judged by their path.
        assert!(!is_mount_root_hidden(false, Some("/a/.b")));
        assert!(is_mount_non_local(None));
        assert!(is_mount_non_local(Some(Some("network"))));
        assert!(!is_mount_non_local(Some(Some("device"))));
        assert!(!is_mount_non_local(Some(None)));
        let prefs = Prefs::default();
        assert!(prefs.should_scan(false));
        assert!(!prefs.should_scan(true));
        let never = Prefs {
            autorun_never: true,
            ..Prefs::default()
        };
        assert!(!never.should_scan(false));
    }

    #[test]
    fn autorun_follows_the_first_content_types_setting() {
        let prefs = Prefs {
            start_app: types(&["x-content/image-dcf"]),
            ignore: types(&["x-content/audio-player"]),
            open_folder: types(&["x-content/unix-software"]),
            ..Prefs::default()
        };
        assert_eq!(
            plan(
                &prefs,
                true,
                &types(&["x-content/image-dcf", "x-content/audio-player"])
            ),
            AutorunPlan::Launch("x-content/image-dcf".into())
        );
        // Only the first type counts.
        assert_eq!(
            plan(
                &prefs,
                true,
                &types(&["x-content/audio-player", "x-content/image-dcf"])
            ),
            AutorunPlan::Nothing
        );
        assert_eq!(
            plan(&prefs, true, &types(&["x-content/unix-software"])),
            AutorunPlan::Launch(DIRECTORY_TYPE.into())
        );
        assert_eq!(
            plan(&prefs, true, &types(&["x-content/video-dvd"])),
            AutorunPlan::Ask
        );
        // No content type: ask.
        assert_eq!(plan(&prefs, true, &[]), AutorunPlan::Ask);
        // A type listed twice: start-app wins over ignore over open-folder.
        let both = Prefs {
            start_app: types(&["t"]),
            ignore: types(&["t", "u"]),
            open_folder: types(&["t", "u", "v"]),
            ..Prefs::default()
        };
        assert_eq!(both.setting_for("t"), AutorunSetting::Run);
        assert_eq!(both.setting_for("u"), AutorunSetting::Ignore);
        assert_eq!(both.setting_for("v"), AutorunSetting::Files);
    }

    #[test]
    fn autorun_never_and_unallowed_mounts_do_nothing() {
        let never = Prefs {
            autorun_never: true,
            start_app: types(&["x-content/image-dcf"]),
            ..Prefs::default()
        };
        assert_eq!(
            plan(&never, true, &types(&["x-content/image-dcf"])),
            AutorunPlan::Nothing
        );
        assert_eq!(plan(&never, true, &[]), AutorunPlan::Nothing);
        assert_eq!(
            plan(&Prefs::default(), false, &types(&["x-content/image-dcf"])),
            AutorunPlan::Nothing
        );
    }

    #[test]
    fn windows_software_is_never_offered() {
        assert_eq!(
            filter_content_types(types(&[
                "x-content/win32-software",
                "x-content/unix-software"
            ])),
            types(&["x-content/unix-software"])
        );
    }

    #[test]
    fn notification_offers_each_default_app() {
        let n = notice("STICK", &types(&["Files", "Photos"]));
        assert_eq!(n.title, "“STICK” connected");
        assert_eq!(n.body, "Disk can now be used");
        assert_eq!(
            n.actions,
            vec![
                ("open-0".to_owned(), "Open with Files".to_owned()),
                ("open-1".to_owned(), "Open with Photos".to_owned()),
            ]
        );
    }

    #[test]
    fn wrong_passphrases_ask_again_other_failures_do_not() {
        assert!(needs_password_again(
            "Error unlocking /dev/sdb1: Failed to activate device: Incorrect passphrase"
        ));
        assert!(needs_password_again("No key available to unlock device"));
        assert!(needs_password_again(
            "Failed to load device's parameters: Operation not permitted"
        ));
        assert!(!needs_password_again("Error mounting: wrong fs type"));
        assert!(pim_unsupported("Compiled against a version of libcryptsetup that does not support the VeraCrypt PIM setting"));
        assert!(!pim_unsupported("Incorrect passphrase"));
    }
}
