//! Screenshots (#61): GNOME Shell's `org.gnome.Shell.Screenshot`.
//!
//! xdg-desktop-portal-gnome's Screenshot portal (and the quick-settings
//! button) call this interface, so screenshots work the GNOME way with
//! the stock portal, as niri does it. The D-Bus side runs on its own
//! thread and hands each request to the event loop over a calloop
//! channel; the loop renders the scene offscreen, saves a PNG, and
//! answers. In the nested preview the host's GNOME Shell owns the name,
//! so this service stays down there.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Bus name and object GNOME Shell serves.
pub const NAME: &str = "org.gnome.Shell.Screenshot";
pub const PATH: &str = "/org/gnome/Shell/Screenshot";
/// Longest a caller waits for the frame.
const DEADLINE: Duration = Duration::from_secs(10);

/// One request from D-Bus.
pub enum Request {
    /// The screen or the focused window, saved where asked (empty:
    /// GNOME's default).
    Shot {
        filename: PathBuf,
        /// The focused window alone (`ScreenshotWindow`), not the screen.
        window: bool,
        flash: bool,
        reply: mpsc::Sender<Option<PathBuf>>,
    },
    /// Every window of the screenshot UI's window selector, each saved
    /// into `directory` (Tuna Desktop's `ScreenshotWindows`).
    Windows {
        directory: PathBuf,
        reply: mpsc::Sender<Vec<WindowShot>>,
    },
    /// Authorized screenshot feedback without a new capture (the shell UI).
    Flash {
        area: [i32; 4],
        reply: mpsc::Sender<bool>,
    },
}

/// One window of GNOME's screenshot window selector: the window, where
/// its preview sits (primary-output logical pixels, laid out as GNOME's
/// `UIWindowSelectorLayout` does) and its picture at full size.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowShot {
    pub id: u64,
    pub title: String,
    pub focused: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub path: PathBuf,
}

/// GNOME's window selector container margins
/// (`.screenshot-ui-window-selector-window-container`): 100px, and 200px
/// at the bottom of the primary monitor to leave room for the panel.
pub const SELECTOR_MARGIN: i32 = 100;
pub const SELECTOR_MARGIN_BOTTOM: i32 = 200;

/// Where the window selector lays windows out on a `width` x `height`
/// output (logical pixels): `(x, y, width, height)`.
pub fn selector_area(width: i32, height: i32) -> (i32, i32, i32, i32) {
    (
        SELECTOR_MARGIN,
        SELECTOR_MARGIN,
        (width - 2 * SELECTOR_MARGIN).max(1),
        (height - SELECTOR_MARGIN - SELECTOR_MARGIN_BOTTOM).max(1),
    )
}

struct Service {
    authority: crate::capture_security::Authority,
    to_loop: calloop::channel::Sender<Request>,
}

/// Tuna Desktop's own addition on the same object: the screenshot UI's window
/// selector needs every window's picture, which GNOME Shell takes
/// in-process (`paint_to_content`) and never writes anywhere. Only the
/// supervised shell, GNOME Shell's in-process role, may call it, and the
/// pictures go only to the compositor's private runtime directory
/// ([`WINDOWS_DIR`]), the shell's stand-in for GNOME's in-memory content.
struct WindowsService {
    authority: crate::capture_security::Authority,
    to_loop: calloop::channel::Sender<Request>,
    /// The compositor's `$XDG_RUNTIME_DIR`, read once at start.
    runtime_dir: Option<PathBuf>,
}

/// One window as `ScreenshotWindows` answers it: id, title, focused,
/// x, y, width, height, path.
pub type WindowShotReply = (u64, String, bool, i32, i32, i32, i32, String);

#[zbus::interface(name = "org.tuna.Screenshot")]
impl WindowsService {
    /// `(directory) -> a(tsbiiiis)`: the active workspace's windows,
    /// each saved as a PNG into `directory`, with their selector slots.
    async fn screenshot_windows(
        &self,
        directory: String,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<Vec<WindowShotReply>> {
        self.authority.unlocked()?;
        let (_, caller) = self.authority.authenticate_caller(conn, &header).await?;
        admit_windows(caller, crate::lockdown::read_save_to_disk_key)?;
        let directory = windows_directory(Path::new(&directory), self.runtime_dir.as_deref())?;
        self.authority.unlocked()?;
        let (reply, answer) = mpsc::channel();
        self.to_loop
            .send(Request::Windows { directory, reply })
            .map_err(|_| zbus::fdo::Error::Failed("compositor gone".into()))?;
        let shots = answer
            .recv_timeout(DEADLINE)
            .map_err(|_| zbus::fdo::Error::Failed("window screenshots failed".into()))?;
        Ok(shots
            .into_iter()
            .map(|s| {
                (
                    s.id,
                    s.title,
                    s.focused,
                    s.x,
                    s.y,
                    s.width,
                    s.height,
                    s.path.to_string_lossy().into_owned(),
                )
            })
            .collect())
    }
}

#[zbus::interface(name = "org.gnome.Shell.Screenshot")]
impl Service {
    async fn flash_area(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.authority.authenticate(conn, &header).await?;
        self.authority
            .unlocked()
            .map_err(|_| zbus::fdo::Error::Failed("session locked".into()))?;
        let (reply, answer) = mpsc::channel();
        self.to_loop
            .send(Request::Flash {
                area: [x, y, width, height],
                reply,
            })
            .map_err(|_| zbus::fdo::Error::Failed("compositor gone".into()))?;
        match answer.recv_timeout(DEADLINE) {
            Ok(true) => Ok(()),
            _ => Err(zbus::fdo::Error::Failed(
                "invalid area or unavailable feedback".into(),
            )),
        }
    }

    /// GNOME Shell's signature: `(include_cursor, flash, filename) ->
    /// (success, filename_used)`.
    async fn screenshot(
        &self,
        _include_cursor: bool,
        flash: bool,
        filename: String,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<(bool, String)> {
        let (_, caller) = self.authority.authenticate_caller(conn, &header).await?;
        refuse_without_disk(caller, crate::lockdown::read_save_to_disk_key)?;
        if self.authority.unlocked().is_err() {
            return Ok((false, String::new()));
        }
        self.request(filename, false, flash)
    }

    /// `(include_frame, include_cursor, flash, filename)`: the focused
    /// window. Tuna Desktop windows draw their own frames, so it is always in.
    async fn screenshot_window(
        &self,
        _include_frame: bool,
        _include_cursor: bool,
        flash: bool,
        filename: String,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<(bool, String)> {
        let (_, caller) = self.authority.authenticate_caller(conn, &header).await?;
        refuse_without_disk(caller, crate::lockdown::read_save_to_disk_key)?;
        if self.authority.unlocked().is_err() {
            return Ok((false, String::new()));
        }
        self.request(filename, true, flash)
    }
}

/// Administrative lockdown (#357): GNOME 51's `screenshot.js`
/// `_createScreenshot` answers `Screenshot`, `ScreenshotWindow` and
/// `ScreenshotArea` with "Saving to disk is disabled" while
/// `org.gnome.desktop.lockdown disable-save-to-disk` is set. Its own
/// screenshot UI and keys are in-process there and keep working
/// clipboard-only; the supervised shell is that in-process role here
/// (it enforces the clipboard-only rule itself), so only the portal —
/// GNOME's external D-Bus caller — is refused. An unreadable key reads
/// GNOME's default (allowed), the lockdown module's refresh rule.
pub fn refuse_without_disk(
    caller: crate::capture_security::CaptureCaller,
    save_to_disk_disabled: impl FnOnce() -> Option<bool>,
) -> zbus::fdo::Result<()> {
    if caller == crate::capture_security::CaptureCaller::Portal
        && save_to_disk_disabled() == Some(true)
    {
        return Err(zbus::fdo::Error::AccessDenied(
            "Saving to disk is disabled".into(),
        ));
    }
    Ok(())
}

/// Who may call `ScreenshotWindows`. GNOME 51's `screenshot.js`
/// `_createScreenshot` checks `disable-save-to-disk` before its caller
/// allowlist, so a portal call under lockdown gets GNOME's lockdown error
/// first ([`refuse_without_disk`]). Then only the supervised shell
/// passes: GNOME's window selector is in-process and has no D-Bus caller.
pub fn admit_windows(
    caller: crate::capture_security::CaptureCaller,
    save_to_disk_disabled: impl FnOnce() -> Option<bool>,
) -> zbus::fdo::Result<()> {
    refuse_without_disk(caller, save_to_disk_disabled)?;
    if caller != crate::capture_security::CaptureCaller::Shell {
        return Err(crate::capture_security::denied(
            "window screenshots are the supervised shell's",
        ));
    }
    Ok(())
}

/// The only directory `ScreenshotWindows` writes to, under
/// `$XDG_RUNTIME_DIR` (the shell's `screenshot_ui.rs` asks for the same).
pub const WINDOWS_DIR: &str = "tuna-screenshot-windows";

/// `requested` when it is exactly `runtime_dir`/[`WINDOWS_DIR`]: a private
/// per-session tmpfs directory, never a persistent location on disk.
/// Paths are compared by component, so `..` or another directory fails.
pub fn windows_directory(
    requested: &Path,
    runtime_dir: Option<&Path>,
) -> zbus::fdo::Result<PathBuf> {
    let expected = runtime_dir
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(WINDOWS_DIR))
        .ok_or_else(|| zbus::fdo::Error::Failed("XDG_RUNTIME_DIR is unset".into()))?;
    if requested != expected {
        return Err(zbus::fdo::Error::InvalidArgs(format!(
            "directory must be {}",
            expected.display()
        )));
    }
    Ok(expected)
}

/// Create `directory` mode 0700 when missing and check it is a real
/// directory (not a symlink) the compositor's user owns and nobody else
/// can enter, so window pictures cannot be redirected out of it.
pub fn private_directory(directory: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(directory) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(directory)?;
    if !meta.file_type().is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "window screenshot directory is not private",
        ));
    }
    Ok(())
}

impl Service {
    fn request(
        &self,
        filename: String,
        window: bool,
        flash: bool,
    ) -> zbus::fdo::Result<(bool, String)> {
        let (reply, answer) = mpsc::channel();
        self.to_loop
            .send(Request::Shot {
                filename: PathBuf::from(filename),
                window,
                flash,
                reply,
            })
            .map_err(|_| zbus::fdo::Error::Failed("compositor gone".into()))?;
        match answer.recv_timeout(DEADLINE) {
            Ok(Some(path)) => Ok((true, path.to_string_lossy().into_owned())),
            // Capture rechecks lock at frame delivery. A lock arriving after
            // admission (or another normal capture failure) returns the same
            // documented failure tuple, without exposing a filename.
            _ => Ok((false, String::new())),
        }
    }
}

/// Serve the interface on the session bus from a thread. Returns the
/// receiving end for the event loop; the thread ends quietly when there
/// is no bus or the name is taken (a real GNOME Shell is running).
pub fn start(authority: crate::capture_security::Authority) -> calloop::channel::Channel<Request> {
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _ = std::thread::Builder::new()
        .name("tuna-screenshot".into())
        .spawn(move || {
            let conn = match zbus::block_on(
                zbus::connection::Builder::session()
                    .serve_at(
                        PATH,
                        Service {
                            authority: authority.clone(),
                            to_loop: to_loop.clone(),
                        },
                    )
                    .serve_at(
                        PATH,
                        WindowsService {
                            to_loop,
                            authority,
                            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
                        },
                    )
                    .build(),
            ) {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-compositor: screenshot service: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DO_NOT_QUEUE;
            match zbus::block_on(conn.request_name_with_flags(NAME, flags)) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("tuna-compositor: screenshot service on {NAME}");
                    // zbus's runtime keeps serving the connection in the
                    // background while this thread parks; keep it alive.
                    loop {
                        std::thread::park();
                    }
                }
                _ => eprintln!("tuna-compositor: {NAME} is taken; screenshot service off"),
            }
        });
    from_dbus
}

/// Where GNOME saves a screenshot: `~/Pictures/Screenshots/Screenshot
/// From YYYY-MM-DD HH-MM-SS.png` (XDG pictures dir when known).
pub fn default_path(now: &jiff_like::Stamp) -> Option<PathBuf> {
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Pictures")))?;
    Some(
        pictures
            .join("Screenshots")
            .join(format!("Screenshot From {}.png", now.0)),
    )
}

/// The requested filename when absolute, else GNOME's default.
pub fn target_path(requested: &Path, now: &jiff_like::Stamp) -> Option<PathBuf> {
    if requested.is_absolute() {
        return Some(requested.to_owned());
    }
    default_path(now)
}

/// A local wall-clock stamp, `YYYY-MM-DD HH-MM-SS` (no time crate here).
pub mod jiff_like {
    pub struct Stamp(pub String);

    impl Stamp {
        /// Now, via `date` for the local zone; UTC seconds as a fallback.
        pub fn now() -> Self {
            let out = std::process::Command::new("date")
                .arg("+%Y-%m-%d %H-%M-%S")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned());
            Self(out.unwrap_or_else(|| {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("{secs}")
            }))
        }
    }
}

/// Encode RGBA8 rows (top-down) as a PNG at `path`, creating its dir.
pub fn save_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_gnome() {
        let stamp = jiff_like::Stamp("2026-10-01 17-50-00".into());
        assert_eq!(
            target_path(Path::new("/tmp/x.png"), &stamp),
            Some(PathBuf::from("/tmp/x.png"))
        );
        std::env::set_var("XDG_PICTURES_DIR", "/home/u/Pictures");
        assert_eq!(
            target_path(Path::new(""), &stamp),
            Some(PathBuf::from(
                "/home/u/Pictures/Screenshots/Screenshot From 2026-10-01 17-50-00.png"
            ))
        );
    }

    #[test]
    fn the_window_selector_leaves_room_for_the_panel() {
        assert_eq!(selector_area(1280, 800), (100, 100, 1080, 500));
        assert_eq!(selector_area(100, 100), (100, 100, 1, 1));
    }

    #[test]
    fn save_to_disk_lockdown_refuses_only_the_portal() {
        use crate::capture_security::CaptureCaller;
        // Denied: the portal (GNOME's external caller) gets GNOME's error.
        let err = refuse_without_disk(CaptureCaller::Portal, || Some(true)).unwrap_err();
        assert!(
            err.to_string().contains("Saving to disk is disabled"),
            "{err}"
        );
        // Allowed, or unreadable (GNOME default): the portal passes.
        assert!(refuse_without_disk(CaptureCaller::Portal, || Some(false)).is_ok());
        assert!(refuse_without_disk(CaptureCaller::Portal, || None).is_ok());
        // The shell is GNOME Shell's in-process role: never refused here
        // (it keeps captures on the clipboard itself), never even reads.
        assert!(refuse_without_disk(CaptureCaller::Shell, || panic!("no read")).is_ok());
    }

    #[test]
    fn window_screenshots_are_the_shells_alone_and_honour_save_to_disk_lockdown() {
        use crate::capture_security::CaptureCaller;
        // The shell: GNOME's in-process selector, never refused, never reads.
        assert!(admit_windows(CaptureCaller::Shell, || panic!("no read")).is_ok());
        // The portal under lockdown: GNOME's lockdown error comes first.
        let err = admit_windows(CaptureCaller::Portal, || Some(true)).unwrap_err();
        assert!(
            err.to_string().contains("Saving to disk is disabled"),
            "{err}"
        );
        // The portal otherwise: refused, it is not the window selector.
        for lockdown in [Some(false), None] {
            let err = admit_windows(CaptureCaller::Portal, || lockdown).unwrap_err();
            assert!(matches!(err, zbus::fdo::Error::AccessDenied(_)), "{err}");
        }
    }

    #[test]
    fn window_screenshots_go_only_to_the_private_runtime_directory() {
        let run = Path::new("/run/user/1000");
        let own = run.join(WINDOWS_DIR);
        assert_eq!(windows_directory(&own, Some(run)).unwrap(), own);
        for requested in [
            "/home/u/Pictures",
            "/run/user/1000",
            "/run/user/1000/tuna-screenshot-windows/sub",
            "/run/user/1000/tuna-screenshot-windows/..",
            "/run/user/1000/other/../tuna-screenshot-windows",
            "/run/user/1001/tuna-screenshot-windows",
            "tuna-screenshot-windows",
            "",
        ] {
            assert!(
                windows_directory(Path::new(requested), Some(run)).is_err(),
                "{requested} accepted"
            );
        }
        // No (or a relative) runtime directory: nowhere is allowed.
        assert!(windows_directory(&own, None).is_err());
        let relative = Path::new("run");
        assert!(windows_directory(&relative.join(WINDOWS_DIR), Some(relative)).is_err());
    }

    #[test]
    fn window_screenshot_directory_is_private_and_never_a_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(WINDOWS_DIR);
        private_directory(&dir).unwrap();
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        // Reused as is.
        private_directory(&dir).unwrap();
        // Opened to others: refused, not silently reused.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&dir).is_err());
        // A planted symlink to a private directory elsewhere: refused.
        let elsewhere = root.path().join("Pictures");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(private_directory(&link).is_err());
        // A plain file in its place: refused.
        let file = root.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(private_directory(&file).is_err());
    }

    /// The real `org.tuna.Screenshot` object on a private bus: only the
    /// supervised shell, only while unlocked, and only into the private
    /// runtime directory reaches the compositor loop.
    #[test]
    fn screenshot_windows_over_the_bus_admits_only_the_shell_into_its_directory() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private test bus starts"));
        let mut address = String::new();
        std::io::BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let connect = || {
            zbus::block_on(
                zbus::connection::Builder::address(address.as_str())
                    .method_timeout(Duration::from_secs(5))
                    .build(),
            )
            .unwrap()
        };

        let run = tempfile::tempdir().unwrap();
        let own = run.path().join(WINDOWS_DIR);
        let authority = crate::capture_security::Authority::default();
        let (to_loop, from_dbus) = calloop::channel::channel();
        let server = connect();
        zbus::block_on(server.object_server().at(
            PATH,
            WindowsService {
                authority: authority.clone(),
                to_loop,
                runtime_dir: Some(run.path().to_owned()),
            },
        ))
        .unwrap();

        let reached = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let event_loop_thread = std::thread::spawn({
            let (reached, stop) = (reached.clone(), stop.clone());
            move || {
                let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
                event_loop
                    .handle()
                    .insert_source(from_dbus, move |event, _, _| {
                        if let calloop::channel::Event::Msg(Request::Windows { directory, reply }) =
                            event
                        {
                            reached.lock().unwrap().push(directory);
                            let _ = reply.send(Vec::new());
                        }
                    })
                    .unwrap();
                while !stop.load(Ordering::SeqCst) {
                    event_loop
                        .dispatch(Some(Duration::from_millis(20)), &mut ())
                        .unwrap();
                }
            }
        });

        let caller = connect();
        // The shell is the bus names its pinned control session declared
        // (ADR 0010); this test's caller connection stands in for it.
        let shell = caller.unique_name().unwrap().to_string();
        let server_name = server.unique_name().unwrap().to_string();
        let call = |directory: &Path| {
            zbus::block_on(caller.call_method(
                Some(server_name.as_str()),
                PATH,
                Some("org.tuna.Screenshot"),
                "ScreenshotWindows",
                &(directory.to_string_lossy().as_ref(),),
            ))
        };
        let error_name = |result: zbus::Result<zbus::Message>| match result {
            Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
            Err(zbus::Error::FDO(error)) => zbus::DBusError::name(&*error).to_string(),
            other => panic!("expected an error, got {other:?}"),
        };

        // Not the shell (and no portal): denied.
        authority.publish(false, std::iter::empty());
        assert_eq!(
            error_name(call(&own)),
            "org.freedesktop.DBus.Error.AccessDenied"
        );
        // The shell, but anywhere else: refused.
        authority.publish(false, [shell.as_str()]);
        for elsewhere in [
            PathBuf::from("/home/u/Pictures"),
            run.path().to_owned(),
            own.join(".."),
        ] {
            assert_eq!(
                error_name(call(&elsewhere)),
                "org.freedesktop.DBus.Error.InvalidArgs",
                "{}",
                elsewhere.display()
            );
        }
        // The shell while locked: denied.
        authority.publish(true, [shell.as_str()]);
        assert_eq!(
            error_name(call(&own)),
            "org.freedesktop.DBus.Error.AccessDenied"
        );
        // The shell, unlocked, into its own directory: reaches the loop.
        authority.publish(false, [shell.as_str()]);
        call(&own).unwrap();

        stop.store(true, Ordering::SeqCst);
        event_loop_thread.join().unwrap();
        assert_eq!(*reached.lock().unwrap(), vec![own]);
    }

    #[test]
    fn png_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b.png");
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        save_png(&path, 2, 1, &rgba).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&buf[..8], &rgba);
    }
}
