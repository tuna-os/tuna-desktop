use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

#[test]
fn blackbody_follows_colords_planckian_table() {
    assert_eq!(blackbody_rgb(6500), [1.0, 1.0, 1.0]);
    assert_eq!(blackbody_rgb(1000), [1.0, 0.1817, 0.0]);
    assert_eq!(blackbody_rgb(10000), [0.7898, 0.8649, 1.0]);
    assert_eq!(blackbody_rgb(500), blackbody_rgb(1000));
    assert_eq!(blackbody_rgb(20000), blackbody_rgb(10000));
    // 2750 K interpolates halfway between 2700 K and 2800 K.
    let (a, b, mid) = (
        blackbody_rgb(2700),
        blackbody_rgb(2800),
        blackbody_rgb(2750),
    );
    for c in 0..3 {
        assert!((mid[c] - (a[c] + b[c]) / 2.0).abs() < 1e-12);
    }
}

#[test]
fn uncalibrated_ramp_matches_mutter_integer_math() {
    let lut = generate_gamma_lut(None, 6500, 256);
    assert_eq!(lut.len(), 256);
    for i in 0..256usize {
        let expected = ((i * 0xffff) / 255) as u16;
        assert_eq!(lut.red[i], expected);
        assert_eq!(lut.green[i], expected);
        assert_eq!(lut.blue[i], expected);
    }
    let warm = generate_gamma_lut(None, 1000, 256);
    assert_eq!(warm.red[255], 0xffff);
    assert_eq!(warm.green[255], (65535.0 * 0.1817) as u16);
    assert_eq!(warm.blue[255], 0);
}

#[test]
fn calibrated_ramp_scales_the_vcgt_by_the_blackbody() {
    let half: Vec<u16> = (0..1024u32)
        .map(|i| (i * 65535 / 1023 / 2) as u16)
        .collect();
    let vcgt = Vcgt([
        icc::Curve::Table(half.clone()),
        icc::Curve::Table(half.clone()),
        icc::Curve::Table(half),
    ]);
    let lut = generate_gamma_lut(Some(&vcgt), 6500, 256);
    assert!((i32::from(lut.red[255]) - 32767).abs() <= 1);
    assert!((i32::from(lut.green[128]) - 16447).abs() <= 2);
    let warm = generate_gamma_lut(Some(&vcgt), 4000, 256);
    let bb = blackbody_rgb(4000);
    assert!((f64::from(warm.blue[255]) - 32767.5 * bb[2]).abs() <= 2.0);
}

#[test]
fn ctm_is_a_clamped_diagonal_in_s31_32() {
    let m = ctm_from_scales([1.0, 0.5, 2.0]);
    assert_eq!(m[0], 1u64 << 32);
    assert_eq!(m[4], 1u64 << 31);
    assert_eq!(m[8], 1u64 << 32);
    assert_eq!(ctm_from_scales([f64::NAN, -1.0, 0.0])[..], [0u64; 9][..]);
    for (i, v) in m.iter().enumerate() {
        if i % 4 != 0 {
            assert_eq!(*v, 0);
        }
    }
}

#[test]
fn white_point_skips_identity_until_something_was_programmed() {
    let lut = CrtcColorCaps {
        gamma_lut_size: 256,
        ctm: true,
    };
    let mut wp = WhitePoint::default();
    // Neutral, no profile, nothing programmed: no KMS update at all.
    assert_eq!(
        wp.update(lut, false, None, None, 6500),
        WhitePointUpdate::Unchanged
    );
    // Night Light warms the ramp.
    let WhitePointUpdate::Program(Programming::GammaLut(warm)) =
        wp.update(lut, false, None, None, 3000)
    else {
        panic!("warm ramp expected");
    };
    assert_eq!(warm, generate_gamma_lut(None, 3000, 256));
    // The same frame again programs nothing.
    assert_eq!(
        wp.update(lut, false, None, None, 3000),
        WhitePointUpdate::Unchanged
    );
    // Back to neutral clears the white point with the identity ramp once.
    assert_eq!(
        wp.update(lut, false, None, None, 6500),
        WhitePointUpdate::Program(Programming::GammaLut(generate_gamma_lut(None, 6500, 256)))
    );
    assert_eq!(
        wp.update(lut, false, None, None, 6500),
        WhitePointUpdate::Unchanged
    );
    // An assigned profile always programs.
    let mut fresh = WhitePoint::default();
    assert!(matches!(
        fresh.update(lut, true, None, None, 6500),
        WhitePointUpdate::Program(Programming::GammaLut(_))
    ));
    // A lost programming is restored on the next update.
    fresh.invalidate();
    assert!(matches!(
        fresh.update(lut, true, None, None, 6500),
        WhitePointUpdate::Program(_)
    ));
}

#[test]
fn white_point_falls_back_to_ctm_then_to_the_renderer() {
    let ctm = CrtcColorCaps {
        gamma_lut_size: 0,
        ctm: true,
    };
    let mut wp = WhitePoint::default();
    assert_eq!(
        wp.update(ctm, false, None, None, 2000),
        WhitePointUpdate::Program(Programming::Ctm(ctm_from_scales(blackbody_rgb(2000))))
    );
    let none = CrtcColorCaps::default();
    assert_eq!(
        WhitePoint::default().update(none, false, None, None, 2000),
        WhitePointUpdate::Unsupported
    );
}

#[test]
fn calibration_sessions_program_raw_ramps_at_the_default_temperature() {
    let caps = CrtcColorCaps {
        gamma_lut_size: 4,
        ctm: false,
    };
    let mut wp = WhitePoint::default();
    // A session without a ramp yet: the profile ramp at 6500 K, even
    // while Night Light asks for 2000 K.
    assert_eq!(
        wp.update(caps, false, None, Some(None), 2000),
        WhitePointUpdate::Program(Programming::GammaLut(generate_gamma_lut(None, 6500, 4)))
    );
    let raw = GammaLut {
        red: vec![0, 1, 2, 3],
        green: vec![4, 5, 6, 7],
        blue: vec![8, 9, 10, 11],
    };
    assert_eq!(
        wp.update(caps, false, None, Some(Some(&raw)), 2000),
        WhitePointUpdate::Program(Programming::GammaLut(raw.clone()))
    );
    // A wrong-size ramp is not programmed.
    let short = GammaLut {
        red: vec![0],
        green: vec![0],
        blue: vec![0],
    };
    assert_eq!(
        wp.update(caps, false, None, Some(Some(&short)), 2000),
        WhitePointUpdate::Program(Programming::GammaLut(generate_gamma_lut(None, 6500, 4)))
    );
    // Session closed: Night Light returns.
    assert_eq!(
        wp.update(caps, false, None, None, 2000),
        WhitePointUpdate::Program(Programming::GammaLut(generate_gamma_lut(None, 2000, 4)))
    );
}

#[test]
fn calibration_session_admission_follows_mutter() {
    let share = ColorShare::default();
    share.set_gamma_sizes([("DP-1".to_owned(), 256), ("DP-2".to_owned(), 0)]);
    assert_eq!(
        share.start_calibration("HDMI-A-9", ":1.5").unwrap_err(),
        "Unknown monitor connector 'HDMI-A-9'"
    );
    assert_eq!(
        share.start_calibration("DP-2", ":1.5").unwrap_err(),
        "Device cannot be calibrated"
    );
    let session = share.start_calibration("DP-1", ":1.5").unwrap();
    assert_eq!(
        session.object_path,
        "/org/gnome/Mutter/ColorManager/Calibration/u1"
    );
    assert_eq!(
        share.start_calibration("DP-1", ":1.6").unwrap_err(),
        "Device is already being calibrated"
    );
    assert_eq!(share.calibration_for("DP-1"), Some(None));
    let lut = GammaLut {
        red: vec![1; 256],
        green: vec![2; 256],
        blue: vec![3; 256],
    };
    share
        .set_calibration_lut(&session.object_path, lut.clone())
        .unwrap();
    assert_eq!(share.calibration_for("DP-1"), Some(Some(lut)));
    // The owner leaving closes its session.
    assert_eq!(
        share.owner_vanished(":1.5"),
        vec![session.object_path.clone()]
    );
    assert_eq!(share.calibration_for("DP-1"), None);
    assert!(share
        .set_calibration_lut(
            &session.object_path,
            GammaLut {
                red: vec![],
                green: vec![],
                blue: vec![]
            }
        )
        .is_err());
    // With colord connected, a device must have finished its setup.
    share.set_status(ColordStatus::Connected);
    share.set_devices(
        0,
        vec![DeviceSnapshot {
            connector: "DP-1".to_owned(),
            ready: false,
            ..Default::default()
        }],
    );
    assert_eq!(
        share.start_calibration("DP-1", ":1.7").unwrap_err(),
        "Device is not ready"
    );
}

#[test]
fn device_properties_match_generate_color_device_props() {
    let edid = crate::edid::tests::sample_edid();
    let panel = MonitorIdentity::new("eDP-1", Some(&edid), true, (294, 165), |_| {
        Some("Dell Inc.".to_owned())
    });
    let props = device_properties(&panel);
    assert_eq!(props["Kind"], "display");
    assert_eq!(props["Mode"], "physical");
    assert_eq!(props["Colorspace"], "rgb");
    assert_eq!(props["Vendor"], "Dell Inc.");
    assert_eq!(props["Model"], "DELL U2415");
    assert_eq!(props["Serial"], "7MT0175A1B2L");
    assert_eq!(props["XRANDR_name"], "eDP-1");
    assert_eq!(props["OutputPriority"], "primary");
    assert_eq!(props["OutputEdidMd5"], crate::edid::md5_hex(&edid));
    assert_eq!(props["Embedded"], "");
    let virtual_ = MonitorIdentity::new("Virtual-1", None, false, (0, 0), |_| None);
    let props = device_properties(&virtual_);
    assert_eq!(props["Vendor"], "");
    assert_eq!(props["OutputPriority"], "secondary");
    assert!(!props.contains_key("OutputEdidMd5"));
    assert!(!props.contains_key("Embedded"));
}

#[test]
fn device_profiles_follow_the_color_store_rules() {
    let edid = crate::edid::tests::sample_edid();
    let with_edid = ColordMonitor {
        identity: MonitorIdentity::new("DP-1", Some(&edid), true, (527, 296), |_| None),
        gamma_lut_size: 0,
    };
    let md5 = crate::edid::md5_hex(&edid);
    assert_eq!(
        device_profile_file_name(&with_edid),
        Some(format!("edid-{md5}.icc"))
    );
    let path = Path::new("/icc/edid.icc");
    let profile = device_profile(&with_edid, path).unwrap().unwrap();
    let bytes = profile.encode().unwrap();
    let parsed = icc::Profile::parse(&bytes).unwrap();
    let meta: HashMap<String, String> = parsed.metadata().into_iter().collect();
    assert_eq!(meta["DATA_source"], "edid");
    assert_eq!(meta["EDID_md5"], md5);
    assert_eq!(meta["EDID_model"], "DELL U2415");
    assert_eq!(meta["EDID_mnft"], "DEL");
    assert_eq!(
        meta["MAPPING_device_id"],
        "xrandr-DEL-DELL U2415-7MT0175A1B2L"
    );
    assert_eq!(meta["Filename"], "/icc/edid.icc");
    assert_eq!(parsed.description().as_deref(), Some("DEL 24\""));
    let shaper = parsed.matrix_shaper().unwrap();
    assert_eq!(shaper.trc[0], icc::Curve::Gamma(2.19921875));

    // No EDID: an sRGB profile only when the CRTC has a gamma LUT.
    let bare = |gamma_lut_size| ColordMonitor {
        identity: MonitorIdentity::new("Virtual-1", None, true, (0, 0), |_| None),
        gamma_lut_size,
    };
    assert!(device_profile_file_name(&bare(0)).is_none());
    assert!(device_profile(&bare(0), path).is_none());
    let name = device_profile_file_name(&bare(256)).unwrap();
    assert_eq!(
        name,
        format!("device-{}.icc", crate::edid::md5_hex(b"xrandr-Virtual-1"))
    );
    let srgb = device_profile(&bare(256), path).unwrap().unwrap();
    let parsed = icc::Profile::parse(&srgb.encode().unwrap()).unwrap();
    let meta: HashMap<String, String> = parsed.metadata().into_iter().collect();
    assert_eq!(meta["DATA_source"], "standard");
    assert_eq!(meta["STANDARD_space"], "srgb");
    assert_eq!(meta["MAPPING_device_id"], "xrandr-Virtual-1");

    // Bogus EDID colour characteristics fail like Mutter.
    let mut bogus = edid.clone();
    bogus[27] = 0;
    bogus[25] &= 0x3f;
    let broken = ColordMonitor {
        identity: MonitorIdentity::new("DP-1", Some(&bogus), true, (0, 0), |_| None),
        gamma_lut_size: 256,
    };
    assert!(device_profile(&broken, path)
        .unwrap()
        .unwrap_err()
        .contains("bogus Color Characteristics"));
}

#[test]
fn store_filter_matches_mutters_color_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let icc = icc::DisplayProfile::srgb().encode().unwrap();
    assert!(is_store_profile(&file("calibrated.icc", b"x")));
    assert!(is_store_profile(&file("Calibrated.ICM", b"x")));
    assert!(is_store_profile(&file("noext", &icc)));
    assert!(!is_store_profile(&file(
        "notes.txt",
        b"plain text that is long enough to read"
    )));
    assert!(!is_store_profile(&file(".hidden.icc", &icc)));
    assert!(!is_store_profile(&file("backup.icc~", &icc)));
    assert!(!is_store_profile(&file("edid-abc.icc", &icc)));
    assert!(!is_store_profile(&file("device-abc.icc", &icc)));
}

// ---------------------------------------------------------------------
// A private bus with a colord double: enough of colord 1.4.7's
// CreateDevice / CreateProfileWithFd / FindDeviceById / DeleteDevice /
// MakeProfileDefault and its metadata auto-add (cd_main_auto_add_from_md)
// to exercise the client end to end. The real daemon runs in
// scripts/tuna-drm-smoke (D-COLORD).
// ---------------------------------------------------------------------

struct TestBus {
    child: Child,
    address: String,
}

impl TestBus {
    fn spawn() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private test bus starts");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            child,
            address: address.trim().to_owned(),
        }
    }

    fn connect(&self) -> zbus::blocking::Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .unwrap()
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct FakeState {
    devices: Vec<(String, String, HashMap<String, String>)>,
    profiles: Vec<(String, String, String)>,
    /// Device path → its profiles in order.
    assignments: HashMap<String, Vec<String>>,
    deleted: Vec<String>,
    created_devices: u32,
}

#[derive(Clone, Default)]
struct FakeColord(Arc<Mutex<FakeState>>);

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.ColorManager")]
enum CdError {
    #[zbus(error)]
    ZBus(zbus::Error),
    AlreadyExists(String),
    NotFound(String),
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[zbus::interface(name = "org.freedesktop.ColorManager")]
impl FakeColord {
    fn get_devices(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
        self.0
            .lock()
            .unwrap()
            .devices
            .iter()
            .map(|d| zbus::zvariant::OwnedObjectPath::try_from(d.1.clone()).unwrap())
            .collect()
    }

    async fn create_device(
        &self,
        id: String,
        _scope: String,
        props: HashMap<String, String>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> Result<zbus::zvariant::OwnedObjectPath, CdError> {
        let path = format!("/org/freedesktop/ColorManager/devices/{}", sanitize(&id));
        {
            let mut s = self.0.lock().unwrap();
            if s.devices.iter().any(|d| d.0 == id) {
                return Err(CdError::AlreadyExists(id));
            }
            s.devices.push((id.clone(), path.clone(), props));
            s.created_devices += 1;
            // cd_main_device_auto_add_from_md: existing profiles mapped to it.
            let mapped: Vec<String> = s
                .profiles
                .iter()
                .filter(|p| mapping_device_id(&p.2).as_deref() == Some(id.as_str()))
                .map(|p| p.1.clone())
                .collect();
            s.assignments.insert(path.clone(), mapped);
        }
        server
            .at(
                path.as_str(),
                FakeDevice {
                    state: self.0.clone(),
                    path: path.clone(),
                },
            )
            .await?;
        Ok(zbus::zvariant::OwnedObjectPath::try_from(path).unwrap())
    }

    fn find_device_by_id(&self, id: String) -> Result<zbus::zvariant::OwnedObjectPath, CdError> {
        let s = self.0.lock().unwrap();
        s.devices
            .iter()
            .find(|d| d.0 == id)
            .map(|d| zbus::zvariant::OwnedObjectPath::try_from(d.1.clone()).unwrap())
            .ok_or(CdError::NotFound(id))
    }

    fn find_profile_by_id(&self, id: String) -> Result<zbus::zvariant::OwnedObjectPath, CdError> {
        let s = self.0.lock().unwrap();
        s.profiles
            .iter()
            .find(|p| p.0 == id)
            .map(|p| zbus::zvariant::OwnedObjectPath::try_from(p.1.clone()).unwrap())
            .ok_or(CdError::NotFound(id))
    }

    fn delete_device(&self, path: zbus::zvariant::OwnedObjectPath) {
        let mut s = self.0.lock().unwrap();
        s.devices.retain(|d| d.1 != path.as_str());
        s.deleted.push(path.to_string());
    }

    fn delete_profile(&self, path: zbus::zvariant::OwnedObjectPath) {
        let mut s = self.0.lock().unwrap();
        s.profiles.retain(|p| p.1 != path.as_str());
        s.deleted.push(path.to_string());
    }

    async fn create_profile_with_fd(
        &self,
        id: String,
        _scope: String,
        fd: zbus::zvariant::OwnedFd,
        props: HashMap<String, String>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> Result<zbus::zvariant::OwnedObjectPath, CdError> {
        let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
        let path = format!("/org/freedesktop/ColorManager/profiles/{}", sanitize(&id));
        let filename = props.get("Filename").cloned().unwrap_or_default();
        let mapped_device = {
            let mut s = self.0.lock().unwrap();
            if s.profiles.iter().any(|p| p.0 == id) {
                return Err(CdError::AlreadyExists(id));
            }
            s.profiles
                .push((id.clone(), path.clone(), filename.clone()));
            let device = mapping_device_id_bytes(&bytes)
                .and_then(|dev| s.devices.iter().find(|d| d.0 == dev).map(|d| d.1.clone()));
            if let Some(device) = &device {
                s.assignments
                    .entry(device.clone())
                    .or_default()
                    .push(path.clone());
            }
            device
        };
        let title = icc::Profile::parse(&bytes)
            .ok()
            .and_then(|p| p.description());
        server
            .at(
                path.as_str(),
                FakeProfile {
                    id,
                    filename,
                    title: title.unwrap_or_default(),
                },
            )
            .await?;
        if let Some(device) = mapped_device {
            let device = zbus::zvariant::ObjectPath::try_from(device).unwrap();
            Self::device_changed(&emitter, device).await?;
        }
        Ok(zbus::zvariant::OwnedObjectPath::try_from(path).unwrap())
    }

    #[zbus(signal)]
    async fn device_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        device: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

fn mapping_device_id_bytes(bytes: &[u8]) -> Option<String> {
    icc::Profile::parse(bytes)
        .ok()?
        .metadata()
        .into_iter()
        .find(|(k, _)| k == "MAPPING_device_id")
        .map(|(_, v)| v)
}

fn mapping_device_id(filename: &str) -> Option<String> {
    mapping_device_id_bytes(&std::fs::read(filename).ok()?)
}

struct FakeDevice {
    state: Arc<Mutex<FakeState>>,
    path: String,
}

#[zbus::interface(name = "org.freedesktop.ColorManager.Device")]
impl FakeDevice {
    #[zbus(property)]
    fn profiles(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
        self.state
            .lock()
            .unwrap()
            .assignments
            .get(&self.path)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|p| zbus::zvariant::OwnedObjectPath::try_from(p).unwrap())
            .collect()
    }

    /// GNOME Settings' "Set for all users"/default choice: hard-add and
    /// move first, then colord announces DeviceChanged.
    async fn make_profile_default(
        &self,
        profile: zbus::zvariant::OwnedObjectPath,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        {
            let mut s = self.state.lock().unwrap();
            let list = s.assignments.entry(self.path.clone()).or_default();
            list.retain(|p| p != profile.as_str());
            list.insert(0, profile.to_string());
        }
        let emitter = zbus::object_server::SignalEmitter::new(conn, CD_PATH)?;
        FakeColord::device_changed(
            &emitter,
            zbus::zvariant::ObjectPath::try_from(self.path.as_str()).unwrap(),
        )
        .await?;
        Ok(())
    }
}

struct FakeProfile {
    id: String,
    filename: String,
    title: String,
}

#[zbus::interface(name = "org.freedesktop.ColorManager.Profile")]
impl FakeProfile {
    #[zbus(property)]
    fn profile_id(&self) -> String {
        self.id.clone()
    }
    #[zbus(property)]
    fn filename(&self) -> String {
        self.filename.clone()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.title.clone()
    }
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn serve_fake(bus: &TestBus) -> (FakeColord, zbus::blocking::Connection) {
    let fake = FakeColord::default();
    let conn = bus.connect();
    conn.object_server().at(CD_PATH, fake.clone()).unwrap();
    conn.request_name(CD_SERVICE).unwrap();
    (fake, conn)
}

#[test]
fn colord_client_registers_devices_profiles_and_follows_assignment() {
    let bus = TestBus::spawn();
    let (fake, fake_conn) = serve_fake(&bus);
    let icc_dir = tempfile::tempdir().unwrap();
    let edid = crate::edid::tests::sample_edid();
    let monitors = vec![
        ColordMonitor {
            identity: MonitorIdentity::new("DP-1", Some(&edid), true, (527, 296), |_| None),
            gamma_lut_size: 256,
        },
        ColordMonitor {
            identity: MonitorIdentity::new("Virtual-1", None, false, (0, 0), |_| None),
            gamma_lut_size: 256,
        },
    ];
    // A user profile with calibration already in the store.
    let mut calibrated = icc::DisplayProfile::srgb();
    calibrated.description = "Calibrated".to_owned();
    let half: Vec<u16> = (0..256u32).map(|i| (i * 65535 / 255 / 2) as u16).collect();
    calibrated.vcgt = Some([half.clone(), half.clone(), half]);
    calibrated.metadata.clear();
    let user = icc_dir.path().join("calibrated.icc");
    std::fs::write(&user, calibrated.encode().unwrap()).unwrap();

    let share = Arc::new(ColorShare::default());
    let client = ColordClient::start(
        share.clone(),
        monitors,
        Bus::Address(bus.address.clone()),
        Some(icc_dir.path().to_owned()),
    );
    wait_for("both devices ready", || {
        share.devices().len() == 2 && share.devices().iter().all(|d| d.ready)
    });
    assert_eq!(share.colord_status(), ColordStatus::Connected);
    let dp = share.device("DP-1").unwrap();
    assert_eq!(dp.device_id, "xrandr-DEL-DELL U2415-7MT0175A1B2L");
    assert!(dp.error.is_none(), "{:?}", dp.error);
    // The EDID profile was written, registered and auto-assigned.
    let md5 = crate::edid::md5_hex(&edid);
    let edid_file = icc_dir.path().join(format!("edid-{md5}.icc"));
    assert!(edid_file.exists());
    let written = std::fs::read(&edid_file).unwrap();
    let expected_id = format!("icc-{}", crate::edid::md5_hex(&written));
    assert_eq!(dp.device_profile.as_ref().unwrap().id, expected_id);
    wait_for("EDID profile assigned", || {
        share
            .device("DP-1")
            .and_then(|d| d.assigned)
            .is_some_and(|a| a.id == expected_id)
    });
    assert!(share.device("DP-1").unwrap().calibration.is_none());
    // The virtual connector got the sRGB fallback device profile.
    let virtual_ = share.device("Virtual-1").unwrap();
    assert!(virtual_
        .device_profile
        .as_ref()
        .and_then(|p| p.filename.as_deref())
        .is_some_and(|f| f.contains("/device-")));
    {
        let s = fake.0.lock().unwrap();
        let dp_props = &s.devices.iter().find(|d| d.0 == dp.device_id).unwrap().2;
        assert_eq!(dp_props["XRANDR_name"], "DP-1");
        assert_eq!(dp_props["OutputEdidMd5"], md5);
        // The store profile was registered too.
        assert!(s.profiles.iter().any(|p| p.2 == user.display().to_string()));
    }
    // GNOME Settings makes the calibrated profile the default.
    let user_path = fake
        .0
        .lock()
        .unwrap()
        .profiles
        .iter()
        .find(|p| p.2 == user.display().to_string())
        .unwrap()
        .1
        .clone();
    let device_path = dp.object_path.clone().unwrap();
    fake_conn
        .call_method(
            Some(CD_SERVICE),
            device_path.as_str(),
            Some(CD_DEVICE),
            "MakeProfileDefault",
            &(zbus::zvariant::ObjectPath::try_from(user_path.as_str()).unwrap(),),
        )
        .unwrap();
    wait_for("calibrated profile assigned", || {
        share
            .device("DP-1")
            .is_some_and(|d| d.calibration.is_some())
    });
    let Vcgt(curves) = share
        .device("DP-1")
        .unwrap()
        .calibration
        .as_deref()
        .cloned()
        .unwrap();
    assert!((curves[0].eval(1.0) - 0.5).abs() < 1e-3);
    // A profile dropped into the store later is registered too.
    let later = icc_dir.path().join("later.icc");
    let mut late = icc::DisplayProfile::srgb();
    late.description = "Later".to_owned();
    std::fs::write(&later, late.encode().unwrap()).unwrap();
    wait_for("later profile registered", || {
        fake.0
            .lock()
            .unwrap()
            .profiles
            .iter()
            .any(|p| p.2 == later.display().to_string())
    });
    client.shutdown();
    let s = fake.0.lock().unwrap();
    assert!(s.deleted.contains(&device_path), "{:?}", s.deleted);
    assert!(s.devices.is_empty(), "devices left: {:?}", s.devices);
}

#[test]
fn superseded_monitor_publications_cannot_restore_a_revoked_sink() {
    let share = Arc::new(ColorShare::default());
    let stale = DeviceSnapshot {
        connector: "DP-1".to_owned(),
        device_id: "old-sink".to_owned(),
        ready: true,
        ..Default::default()
    };
    share.set_devices(0, vec![stale.clone()]);
    share.set_gamma_sizes([("DP-1".to_owned(), 4)]);
    let session = share.start_calibration("DP-1", ":1.9").unwrap();
    let acquired = Arc::new(std::sync::Barrier::new(2));
    let revoked = Arc::new(std::sync::Barrier::new(2));
    let worker = {
        let share = share.clone();
        let acquired = acquired.clone();
        let revoked = revoked.clone();
        std::thread::spawn(move || {
            // An old worker event already read its ready/assigned state,
            // then blocked in colord while discovery replaced the sink.
            let generation = share.monitor_generation();
            acquired.wait();
            revoked.wait();
            share.set_devices(generation, vec![stale]);
            share.update_device(generation, "DP-1", |_| {
                panic!("stale registration completed")
            });
        })
    };
    acquired.wait();
    let replacement = ColordMonitor {
        identity: MonitorIdentity::new("DP-1", None, true, (0, 0), |_| None),
        gamma_lut_size: 4,
    };
    let generation = share.begin_monitor_update(&[replacement], &[]);
    revoked.wait();
    worker.join().unwrap();
    assert!(share.device("DP-1").is_none());
    assert!(share.calibration_for("DP-1").is_none());
    assert!(share
        .set_calibration_lut(&session.object_path, generate_gamma_lut(None, 6500, 4))
        .is_err());
    share.set_devices(
        generation,
        vec![DeviceSnapshot {
            connector: "DP-1".to_owned(),
            device_id: "replacement".to_owned(),
            ..Default::default()
        }],
    );
    share.update_device(generation, "DP-1", |device| device.ready = true);
    assert!(share.device("DP-1").unwrap().ready);
}

#[test]
fn hotplug_preserves_unchanged_assignment_and_revokes_removed_calibration() {
    let bus = TestBus::spawn();
    let (_fake, _connection) = serve_fake(&bus);
    let directory = tempfile::tempdir().unwrap();
    let monitor = |connector, primary| ColordMonitor {
        identity: MonitorIdentity::new(connector, None, primary, (0, 0), |_| None),
        gamma_lut_size: 4,
    };
    let dp = monitor("DP-1", true);
    let hdmi = monitor("HDMI-A-1", false);
    let share = Arc::new(ColorShare::default());
    share.set_gamma_sizes([("DP-1".to_owned(), 4)]);
    let mut client = ColordClient::start(
        share.clone(),
        vec![dp.clone()],
        Bus::Address(bus.address.clone()),
        Some(directory.path().to_owned()),
    );
    wait_for("initial assignment", || {
        share
            .device("DP-1")
            .is_some_and(|device| device.ready && device.assigned.is_some())
    });
    let before = share.device("DP-1").unwrap();
    let session = share.start_calibration("DP-1", ":1.22").unwrap();
    let ramp = generate_gamma_lut(None, 6500, 4);
    share
        .set_calibration_lut(&session.object_path, ramp.clone())
        .unwrap();
    client.update_monitors(&share, vec![dp.clone(), hdmi.clone()]);
    wait_for("second monitor registered", || {
        share.device("HDMI-A-1").is_some_and(|device| device.ready)
    });
    let unchanged = share.device("DP-1").unwrap();
    assert_eq!(unchanged.object_path, before.object_path);
    assert_eq!(unchanged.assigned, before.assigned);
    assert_eq!(share.calibration_for("DP-1"), Some(Some(ramp.clone())));
    client.update_monitors(&share, vec![hdmi]);
    assert!(share.calibration_for("DP-1").is_none());
    assert!(share.device("DP-1").is_none());
    assert!(share
        .set_calibration_lut(&session.object_path, ramp.clone())
        .is_err());
    client.update_monitors(&share, vec![dp]);
    wait_for("reconnected sink registered", || {
        share.devices().len() == 1 && share.device("DP-1").is_some_and(|device| device.ready)
    });
    assert!(share.calibration_for("DP-1").is_none());
    assert!(share
        .set_calibration_lut(&session.object_path, ramp)
        .is_err());
    client.shutdown();
}

#[test]
fn colord_client_reuses_an_existing_device() {
    let bus = TestBus::spawn();
    let (fake, _conn) = serve_fake(&bus);
    let identity = MonitorIdentity::new("HDMI-A-1", None, true, (0, 0), |_| None);
    fake.0.lock().unwrap().devices.push((
        identity.colord_device_id(),
        "/org/freedesktop/ColorManager/devices/stale".to_owned(),
        HashMap::new(),
    ));
    let share = Arc::new(ColorShare::default());
    let client = ColordClient::start(
        share.clone(),
        vec![ColordMonitor {
            identity,
            gamma_lut_size: 0,
        }],
        Bus::Address(bus.address.clone()),
        None,
    );
    wait_for("device ready", || {
        share.device("HDMI-A-1").is_some_and(|d| d.ready)
    });
    let device = share.device("HDMI-A-1").unwrap();
    assert_eq!(
        device.object_path.as_deref(),
        Some("/org/freedesktop/ColorManager/devices/stale")
    );
    assert!(device.device_profile.is_none());
    assert_eq!(fake.0.lock().unwrap().created_devices, 0);
    client.shutdown();
}

#[test]
fn colord_absence_is_reported_not_fatal() {
    let bus = TestBus::spawn();
    let share = Arc::new(ColorShare::default());
    let client = ColordClient::start(
        share.clone(),
        vec![ColordMonitor {
            identity: MonitorIdentity::new("DP-1", None, true, (0, 0), |_| None),
            gamma_lut_size: 256,
        }],
        Bus::Address(bus.address.clone()),
        None,
    );
    wait_for("status", || {
        matches!(share.colord_status(), ColordStatus::Unavailable(_))
    });
    assert!(share.devices().is_empty());
    client.shutdown();
}
