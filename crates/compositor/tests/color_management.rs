//! wp_color_manager_v1 (v2) and wp_color_representation_manager_v1 (v1),
//! driven by a real client against the headless compositor (P-DS-03,
//! #341): the supported events, output and feedback descriptions with
//! their information, parametric and ICC descriptions, and Mutter 51's
//! protocol errors.

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use tuna_compositor::windows::WindowManager;
use tuna_compositor::{State, TestCompositor};
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_output::WlOutput,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::{self as cm_output, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{
        self as cm_feedback, WpColorManagementSurfaceFeedbackV1,
    },
    wp_color_management_surface_v1::{self as cm_surface, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self as manager, WpColorManagerV1},
    wp_image_description_creator_icc_v1::{self as creator_icc, WpImageDescriptionCreatorIccV1},
    wp_image_description_creator_params_v1::{
        self as creator_params, WpImageDescriptionCreatorParamsV1,
    },
    wp_image_description_info_v1::{self as info, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self as image_desc, WpImageDescriptionV1},
};
use wayland_protocols::wp::color_representation::v1::client::{
    wp_color_representation_manager_v1::{self as repr_manager, WpColorRepresentationManagerV1},
    wp_color_representation_surface_v1::{self as repr, WpColorRepresentationSurfaceV1},
};

const ROUNDS: usize = 10;

/// What an image description (by its tag) was told.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fate {
    Ready(u64),
    Failed(WEnum<image_desc::Cause>, String),
}

/// One `wp_image_description_info_v1` event, as received.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Info {
    Primaries([i32; 8]),
    PrimariesNamed(WEnum<manager::Primaries>),
    TfNamed(WEnum<manager::TransferFunction>),
    TfPower(u32),
    Luminances(u32, u32, u32),
    TargetPrimaries([i32; 8]),
    TargetLuminance(u32, u32),
    Done,
}

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    intents: Vec<WEnum<manager::RenderIntent>>,
    features: Vec<WEnum<manager::Feature>>,
    tfs: Vec<WEnum<manager::TransferFunction>>,
    primaries: Vec<WEnum<manager::Primaries>>,
    manager_done: bool,
    alpha_modes: Vec<WEnum<repr::AlphaMode>>,
    coefficients: Vec<(WEnum<repr::Coefficients>, WEnum<repr::Range>)>,
    repr_done: bool,
    fates: HashMap<u32, Fate>,
    infos: HashMap<u32, Vec<Info>>,
    output_changes: usize,
    preferred_changes: Vec<u64>,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<WpColorManagerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpColorManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::SupportedIntent { render_intent } => state.intents.push(render_intent),
            manager::Event::SupportedFeature { feature } => state.features.push(feature),
            manager::Event::SupportedTfNamed { tf } => state.tfs.push(tf),
            manager::Event::SupportedPrimariesNamed { primaries } => {
                state.primaries.push(primaries)
            }
            manager::Event::Done => state.manager_done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpColorRepresentationManagerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpColorRepresentationManagerV1,
        event: repr_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            repr_manager::Event::SupportedAlphaMode { alpha_mode } => {
                state.alpha_modes.push(alpha_mode)
            }
            repr_manager::Event::SupportedCoefficientsAndRanges {
                coefficients,
                range,
            } => state.coefficients.push((coefficients, range)),
            repr_manager::Event::Done => state.repr_done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionV1,
        event: image_desc::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            image_desc::Event::Ready { identity } => {
                state.fates.insert(*tag, Fate::Ready(u64::from(identity)));
            }
            image_desc::Event::Ready2 {
                identity_hi,
                identity_lo,
            } => {
                let id = (u64::from(identity_hi) << 32) | u64::from(identity_lo);
                state.fates.insert(*tag, Fate::Ready(id));
            }
            image_desc::Event::Failed { cause, msg } => {
                state.fates.insert(*tag, Fate::Failed(cause, msg));
            }
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionInfoV1,
        event: info::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let event = match event {
            info::Event::Primaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => Info::Primaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
            info::Event::PrimariesNamed { primaries } => Info::PrimariesNamed(primaries),
            info::Event::TfNamed { tf } => Info::TfNamed(tf),
            info::Event::TfPower { eexp } => Info::TfPower(eexp),
            info::Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => Info::Luminances(min_lum, max_lum, reference_lum),
            info::Event::TargetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => Info::TargetPrimaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
            info::Event::TargetLuminance { min_lum, max_lum } => {
                Info::TargetLuminance(min_lum, max_lum)
            }
            info::Event::Done => Info::Done,
            other => panic!("unexpected info event {other:?}"),
        };
        state.infos.entry(*tag).or_default().push(event);
    }
}

impl Dispatch<WpColorManagementOutputV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpColorManagementOutputV1,
        event: cm_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let cm_output::Event::ImageDescriptionChanged = event {
            state.output_changes += 1;
        }
    }
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpColorManagementSurfaceFeedbackV1,
        event: cm_feedback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let identity = match event {
            cm_feedback::Event::PreferredChanged { identity } => u64::from(identity),
            cm_feedback::Event::PreferredChanged2 {
                identity_hi,
                identity_lo,
            } => (u64::from(identity_hi) << 32) | u64::from(identity_lo),
            _ => return,
        };
        state.preferred_changes.push(identity);
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlSubcompositor);
wayland_client::delegate_noop!(Client: ignore WlSubsurface);
wayland_client::delegate_noop!(Client: ignore WlOutput);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore WpColorManagementSurfaceV1);
wayland_client::delegate_noop!(Client: ignore WpImageDescriptionCreatorIccV1);
wayland_client::delegate_noop!(Client: ignore WpImageDescriptionCreatorParamsV1);
wayland_client::delegate_noop!(Client: ignore WpColorRepresentationSurfaceV1);

struct Peer {
    conn: Connection,
    queue: EventQueue<Client>,
    registry: WlRegistry,
    client: Client,
}

impl Peer {
    fn qh(&self) -> QueueHandle<Client> {
        self.queue.handle()
    }

    fn bind<I>(&self, version: u32) -> I
    where
        I: Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        let (name, offered) = self.client.globals[I::interface().name];
        self.registry
            .bind(name, version.min(offered), &self.qh(), ())
    }

    fn error(&self) -> Option<(String, u32)> {
        self.conn
            .protocol_error()
            .map(|e| (e.object_interface, e.code))
    }
}

/// A compositor with one 60 Hz output.
fn compositor() -> (TestCompositor, WindowManager) {
    let mut comp = TestCompositor::new();
    let output = Output::new(
        "tuna-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".to_owned(),
            model: "Test".to_owned(),
        },
    );
    let mode = Mode {
        size: (1280, 800).into(),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    output.create_global::<State>(&comp.display.handle());
    comp.state.add_output("tuna-0", Some(output), 1280, 800);
    let manager = comp.window_manager();
    (comp, manager)
}

fn connect(comp: &mut TestCompositor, manager: &mut WindowManager) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer {
        conn,
        queue,
        registry,
        client: Client::default(),
    };
    pump(comp, manager, &mut peer);
    peer
}

/// Round trips; a protocol error ends the client's side quietly, for the
/// test to read with [`Peer::error`].
fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peer: &mut Peer) {
    for _ in 0..ROUNDS {
        let _ = peer.queue.flush();
        comp.pump();
        manager.reconcile(&mut comp.state);
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        let _ = peer.queue.dispatch_pending(&mut peer.client);
    }
}

fn peer() -> (TestCompositor, WindowManager, Peer) {
    let (mut comp, mut manager) = compositor();
    let peer = connect(&mut comp, &mut manager);
    (comp, manager, peer)
}

/// A file holding `bytes`, as a client hands an ICC profile over.
fn file_with(bytes: &[u8]) -> std::fs::File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

fn params(peer: &Peer, cm: &WpColorManagerV1) -> WpImageDescriptionCreatorParamsV1 {
    cm.create_parametric_creator(&peer.qh(), ())
}

/// An image description from named primaries and transfer function.
fn parametric(
    peer: &Peer,
    cm: &WpColorManagerV1,
    primaries: manager::Primaries,
    tf: manager::TransferFunction,
    tag: u32,
) -> WpImageDescriptionV1 {
    let creator = params(peer, cm);
    creator.set_primaries_named(primaries);
    creator.set_tf_named(tf);
    creator.create(&peer.qh(), tag)
}

/// sRGB primaries chromaticities, scaled and truncated as Mutter sends them.
const SRGB_PRIMARIES: [i32; 8] = [
    640000, 330000, 300000, 600000, 150000, 60000, 312700, 329000,
];

fn assert_srgb_gamma22_sdr(events: &[Info]) {
    assert_eq!(events.last(), Some(&Info::Done), "{events:?}");
    assert!(events.contains(&Info::PrimariesNamed(WEnum::Value(
        manager::Primaries::Srgb
    ))));
    assert!(events.contains(&Info::TfNamed(WEnum::Value(
        manager::TransferFunction::Gamma22
    ))));
    assert!(
        events.contains(&Info::Luminances(2000, 80, 80)),
        "{events:?}"
    );
    assert!(
        events.contains(&Info::TargetLuminance(2000, 80)),
        "{events:?}"
    );
    let primaries = events
        .iter()
        .find_map(|e| match e {
            Info::Primaries(p) => Some(*p),
            _ => None,
        })
        .unwrap();
    for (got, want) in primaries.iter().zip(SRGB_PRIMARIES) {
        assert!((got - want).abs() <= 1, "{primaries:?}");
    }
    assert!(events.contains(&Info::TargetPrimaries(primaries)));
}

#[test]
fn output_encoding_changes_notify_clients_and_preserve_old_descriptions() {
    use tuna_compositor::color_state;
    for version in [1, 2] {
        let (mut comp, mut manager, mut peer) = peer();
        let cm: WpColorManagerV1 = peer.bind(version);
        let compositor: WlCompositor = peer.bind(6);
        let output: WlOutput = peer.bind(4);
        let surface = compositor.create_surface(&peer.qh(), ());
        let feedback = cm.get_surface_feedback(&surface, &peer.qh(), ());
        let color_output = cm.get_output(&output, &peer.qh(), ());
        let old = color_output.get_image_description(&peer.qh(), 11);
        pump(&mut comp, &mut manager, &mut peer);
        old.get_information(&peer.qh(), 11);
        pump(&mut comp, &mut manager, &mut peer);
        let old_identity = peer.client.fates[&11].clone();
        assert_srgb_gamma22_sdr(&peer.client.infos[&11]);
        assert!(peer.client.preferred_changes.is_empty());

        assert!(comp
            .state
            .set_output_color_state("tuna-0", color_state::hdr_output_params()));
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(peer.client.output_changes, 1);
        assert_eq!(peer.client.preferred_changes.len(), 1);
        let new = color_output.get_image_description(&peer.qh(), 12);
        let preferred = feedback.get_preferred(&peer.qh(), 13);
        pump(&mut comp, &mut manager, &mut peer);
        new.get_information(&peer.qh(), 12);
        preferred.get_information(&peer.qh(), 13);
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(peer.client.fates[&12], peer.client.fates[&13]);
        assert_ne!(peer.client.fates[&12], old_identity);
        assert_eq!(peer.client.fates[&11], old_identity);
        for tag in [12, 13] {
            assert!(
                peer.client.infos[&tag].contains(&Info::PrimariesNamed(WEnum::Value(
                    manager::Primaries::Bt2020
                ),))
            );
            assert!(
                peer.client.infos[&tag].contains(&Info::TfNamed(WEnum::Value(
                    manager::TransferFunction::St2084Pq
                ),))
            );
        }
        assert!(!comp
            .state
            .set_output_color_state("tuna-0", color_state::hdr_output_params()));
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(peer.client.output_changes, 1);
        assert_eq!(peer.client.preferred_changes.len(), 1);

        assert!(comp
            .state
            .set_output_color_state("tuna-0", color_state::output_params()));
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(peer.client.output_changes, 2);
        assert_eq!(peer.client.preferred_changes.len(), 2);
        let restored = color_output.get_image_description(&peer.qh(), 14);
        pump(&mut comp, &mut manager, &mut peer);
        restored.get_information(&peer.qh(), 14);
        pump(&mut comp, &mut manager, &mut peer);
        assert_srgb_gamma22_sdr(&peer.client.infos[&14]);
        assert_eq!(peer.error(), None);
    }
}

#[test]
fn removed_output_description_stays_inert_when_connector_name_is_reused() {
    let (mut comp, mut manager, mut peer) = peer();
    let cm: WpColorManagerV1 = peer.bind(2);
    let output: WlOutput = peer.bind(4);
    let old = cm.get_output(&output, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut peer);
    assert!(comp.state.remove_output("tuna-0"));
    let replacement = Output::new(
        "tuna-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".to_owned(),
            model: "Replacement".to_owned(),
        },
    );
    comp.state
        .add_output("tuna-0", Some(replacement), 1280, 800);
    old.get_image_description(&peer.qh(), 15);
    pump(&mut comp, &mut manager, &mut peer);
    assert!(matches!(
        peer.client.fates.get(&15),
        Some(Fate::Failed(WEnum::Value(image_desc::Cause::NoOutput), _))
    ));
    assert_eq!(peer.error(), None);
}

#[test]
fn supported_events_match_mutter() {
    let (mut comp, mut manager, mut peer) = peer();
    assert_eq!(peer.client.globals["wp_color_manager_v1"].1, 2);
    assert_eq!(
        peer.client.globals["wp_color_representation_manager_v1"].1,
        1
    );
    let _cm: WpColorManagerV1 = peer.bind(2);
    let _repr: WpColorRepresentationManagerV1 = peer.bind(1);
    pump(&mut comp, &mut manager, &mut peer);

    let c = &peer.client;
    assert!(c.manager_done && c.repr_done);
    assert_eq!(
        c.intents,
        vec![WEnum::Value(manager::RenderIntent::Perceptual)]
    );
    use manager::Feature as F;
    assert_eq!(
        c.features,
        [
            F::IccV2V4,
            F::Parametric,
            F::SetPrimaries,
            F::SetTfPower,
            F::SetLuminances
        ]
        .map(WEnum::Value)
    );
    use manager::TransferFunction as T;
    assert_eq!(
        c.tfs,
        [
            T::Gamma22,
            T::Gamma28,
            T::St2084Pq,
            T::Bt1886,
            T::ExtLinear,
            T::CompoundPower24
        ]
        .map(WEnum::Value)
    );
    use manager::Primaries as P;
    assert_eq!(
        c.primaries,
        [P::Srgb, P::Bt2020, P::Ntsc, P::Pal, P::DisplayP3].map(WEnum::Value)
    );
    assert_eq!(
        c.alpha_modes,
        [
            repr::AlphaMode::PremultipliedElectrical,
            repr::AlphaMode::Straight
        ]
        .map(WEnum::Value)
    );
    let mut pairs = Vec::new();
    for coefficients in [
        repr::Coefficients::Identity,
        repr::Coefficients::Bt709,
        repr::Coefficients::Bt601,
        repr::Coefficients::Bt2020,
    ] {
        for range in [repr::Range::Full, repr::Range::Limited] {
            pairs.push((WEnum::Value(coefficients), WEnum::Value(range)));
        }
    }
    assert_eq!(c.coefficients, pairs);

    // Version 1 names piece-wise sRGB `srgb`, not `compound_power_2_4`.
    let mut old = connect(&mut comp, &mut manager);
    let _cm: WpColorManagerV1 = old.bind(1);
    pump(&mut comp, &mut manager, &mut old);
    assert_eq!(old.client.tfs.last(), Some(&WEnum::Value(T::Srgb)));
    assert!(!old.client.tfs.contains(&WEnum::Value(T::CompoundPower24)));
}

#[test]
fn output_and_feedback_describe_srgb_gamma22_sdr() {
    let (mut comp, mut manager, mut peer) = peer();
    let qh = peer.qh();
    let cm: WpColorManagerV1 = peer.bind(2);
    let output: WlOutput = peer.bind(4);
    let compositor: WlCompositor = peer.bind(6);
    let cm_output = cm.get_output(&output, &qh, ());
    let from_output = cm_output.get_image_description(&qh, 1);
    let surface = compositor.create_surface(&qh, ());
    let feedback = cm.get_surface_feedback(&surface, &qh, ());
    let preferred = feedback.get_preferred(&qh, 2);
    let parametric = feedback.get_preferred_parametric(&qh, 3);
    pump(&mut comp, &mut manager, &mut peer);

    let Some(Fate::Ready(id)) = peer.client.fates.get(&1).cloned() else {
        panic!("{:?}", peer.client.fates);
    };
    assert_eq!(peer.client.fates.get(&2), Some(&Fate::Ready(id)));
    assert_eq!(peer.client.fates.get(&3), Some(&Fate::Ready(id)));

    from_output.get_information(&qh, 1);
    preferred.get_information(&qh, 2);
    parametric.get_information(&qh, 3);
    pump(&mut comp, &mut manager, &mut peer);
    assert_eq!(peer.error(), None);
    for tag in 1..=3 {
        assert_srgb_gamma22_sdr(&peer.client.infos[&tag]);
    }
}

#[test]
fn parametric_descriptions_are_ready_and_apply_to_surfaces() {
    let (mut comp, mut manager, mut peer) = peer();
    let qh = peer.qh();
    let cm: WpColorManagerV1 = peer.bind(2);
    let compositor: WlCompositor = peer.bind(6);
    let pq = parametric(
        &peer,
        &cm,
        manager::Primaries::Bt2020,
        manager::TransferFunction::St2084Pq,
        1,
    );
    let same = parametric(
        &peer,
        &cm,
        manager::Primaries::Bt2020,
        manager::TransferFunction::St2084Pq,
        2,
    );
    let custom = params(&peer, &cm);
    custom.set_primaries(
        680000, 320000, 265000, 690000, 150000, 60000, 312700, 329000,
    );
    custom.set_tf_power(24000);
    custom.set_luminances(1000, 400, 200);
    let _custom = custom.create(&qh, 3);
    let _srgb = parametric(
        &peer,
        &cm,
        manager::Primaries::Srgb,
        manager::TransferFunction::Gamma22,
        4,
    );
    pump(&mut comp, &mut manager, &mut peer);
    assert_eq!(peer.error(), None);
    let ids: Vec<u64> = (1..=4)
        .map(|tag| match peer.client.fates.get(&tag) {
            Some(Fate::Ready(id)) => *id,
            other => panic!("description {tag}: {other:?}"),
        })
        .collect();
    // Every description is a colour state of its own, as in Mutter: equal
    // parameters still get a new identity.
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");

    let surface = compositor.create_surface(&qh, ());
    let cm_surface = cm.get_surface(&surface, &qh, ());
    cm_surface.set_image_description(&pq, manager::RenderIntent::Perceptual);
    surface.commit();
    cm_surface.unset_image_description();
    surface.commit();
    cm_surface.set_image_description(&same, manager::RenderIntent::Perceptual);
    surface.commit();
    pump(&mut comp, &mut manager, &mut peer);
    assert_eq!(peer.error(), None);
}

#[test]
fn icc_descriptions_are_ready_or_fail_like_mutter() {
    let (mut comp, mut manager, mut peer) = peer();
    let qh = peer.qh();
    let cm: WpColorManagerV1 = peer.bind(2);
    let profile = tuna_compositor::icc::DisplayProfile::srgb()
        .encode()
        .unwrap();
    let mut padded = vec![0u8; 16];
    padded.extend_from_slice(&profile);

    let good = cm.create_icc_creator(&qh, ());
    good.set_icc_file(file_with(&profile).as_fd(), 0, profile.len() as u32);
    let _good = good.create(&qh, 1);
    let offset = cm.create_icc_creator(&qh, ());
    offset.set_icc_file(file_with(&padded).as_fd(), 16, profile.len() as u32);
    let _offset = offset.create(&qh, 2);
    let garbage = cm.create_icc_creator(&qh, ());
    garbage.set_icc_file(file_with(&[0x55; 256]).as_fd(), 0, 256);
    let garbage = garbage.create(&qh, 3);
    pump(&mut comp, &mut manager, &mut peer);
    assert_eq!(peer.error(), None);

    let Some(Fate::Ready(id)) = peer.client.fates.get(&1).cloned() else {
        panic!("{:?}", peer.client.fates);
    };
    assert!(
        matches!(peer.client.fates.get(&2), Some(Fate::Ready(other)) if *other != id),
        "{:?}",
        peer.client.fates
    );
    assert!(
        matches!(
            peer.client.fates.get(&3),
            Some(Fate::Failed(
                WEnum::Value(image_desc::Cause::OperatingSystem),
                _
            ))
        ),
        "{:?}",
        peer.client.fates.get(&3)
    );

    // A failed description cannot be set on a surface.
    let compositor: WlCompositor = peer.bind(6);
    let surface = compositor.create_surface(&qh, ());
    let cm_surface = cm.get_surface(&surface, &qh, ());
    cm_surface.set_image_description(&garbage, manager::RenderIntent::Perceptual);
    pump(&mut comp, &mut manager, &mut peer);
    assert_eq!(
        peer.error(),
        Some((
            "wp_color_management_surface_v1".to_owned(),
            cm_surface::Error::ImageDescription as u32
        ))
    );
}

/// Run `request` on a fresh client and return the protocol error it got.
fn error_of(request: impl FnOnce(&Peer, &WpColorManagerV1)) -> Option<(String, u32)> {
    let (mut comp, mut manager, mut peer) = peer();
    let cm: WpColorManagerV1 = peer.bind(2);
    pump(&mut comp, &mut manager, &mut peer);
    request(&peer, &cm);
    pump(&mut comp, &mut manager, &mut peer);
    peer.error()
}

fn expect(interface: &str, code: u32) -> Option<(String, u32)> {
    Some((interface.to_owned(), code))
}

#[test]
fn color_management_errors_follow_mutter() {
    const MANAGER: &str = "wp_color_manager_v1";
    const PARAMS: &str = "wp_image_description_creator_params_v1";
    const ICC: &str = "wp_image_description_creator_icc_v1";
    const DESC: &str = "wp_image_description_v1";
    const SURFACE: &str = "wp_color_management_surface_v1";

    assert_eq!(
        error_of(|peer, cm| {
            let compositor: WlCompositor = peer.bind(6);
            let surface = compositor.create_surface(&peer.qh(), ());
            cm.get_surface(&surface, &peer.qh(), ());
            cm.get_surface(&surface, &peer.qh(), ());
        }),
        expect(MANAGER, manager::Error::SurfaceExists as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            cm.create_windows_scrgb(&peer.qh(), 0);
        }),
        expect(MANAGER, manager::Error::UnsupportedFeature as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let creator = params(peer, cm);
            creator.set_primaries_named(manager::Primaries::Srgb);
            creator.create(&peer.qh(), 0);
        }),
        // `create` is a destructor: the client has dropped the creator by
        // the time the error names it.
        expect("", creator_params::Error::IncompleteSet as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let creator = params(peer, cm);
            creator.set_tf_named(manager::TransferFunction::Gamma22);
            creator.set_tf_power(22000);
        }),
        expect(PARAMS, creator_params::Error::AlreadySet as u32)
    );
    assert_eq!(
        error_of(|peer, cm| params(peer, cm).set_tf_power(5000)),
        expect(PARAMS, creator_params::Error::InvalidTf as u32)
    );
    assert_eq!(
        error_of(|peer, cm| params(peer, cm).set_tf_named(manager::TransferFunction::Log100)),
        expect(PARAMS, creator_params::Error::InvalidTf as u32)
    );
    assert_eq!(
        error_of(|peer, cm| params(peer, cm).set_primaries_named(manager::Primaries::Cie1931Xyz)),
        expect(PARAMS, creator_params::Error::InvalidPrimariesNamed as u32)
    );
    assert_eq!(
        error_of(|peer, cm| params(peer, cm).set_luminances(10000, 1, 1)),
        expect(PARAMS, creator_params::Error::InvalidLuminance as u32)
    );
    assert_eq!(
        error_of(|peer, cm| params(peer, cm).set_luminances(0, 80, 200)),
        expect(PARAMS, creator_params::Error::InvalidLuminance as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let creator = cm.create_icc_creator(&peer.qh(), ());
            creator.create(&peer.qh(), 0);
        }),
        expect("", creator_icc::Error::IncompleteSet as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let file = file_with(&[0; 64]);
            cm.create_icc_creator(&peer.qh(), ())
                .set_icc_file(file.as_fd(), 0, 0);
        }),
        expect(ICC, creator_icc::Error::BadSize as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let file = file_with(&[0; 64]);
            cm.create_icc_creator(&peer.qh(), ())
                .set_icc_file(file.as_fd(), 32, 64);
        }),
        expect(ICC, creator_icc::Error::OutOfFile as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let file = file_with(&[0; 64]);
            let creator = cm.create_icc_creator(&peer.qh(), ());
            creator.set_icc_file(file.as_fd(), 0, 64);
            creator.set_icc_file(file.as_fd(), 0, 64);
        }),
        expect(ICC, creator_icc::Error::AlreadySet as u32)
    );
    // Only output and feedback descriptions carry information.
    assert_eq!(
        error_of(|peer, cm| {
            let desc = parametric(
                peer,
                cm,
                manager::Primaries::Srgb,
                manager::TransferFunction::Gamma22,
                0,
            );
            desc.get_information(&peer.qh(), 0);
        }),
        expect(DESC, image_desc::Error::NoInformation as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let compositor: WlCompositor = peer.bind(6);
            let surface = compositor.create_surface(&peer.qh(), ());
            let desc = parametric(
                peer,
                cm,
                manager::Primaries::Srgb,
                manager::TransferFunction::Gamma22,
                0,
            );
            cm.get_surface(&surface, &peer.qh(), ())
                .set_image_description(&desc, manager::RenderIntent::Relative);
        }),
        expect(SURFACE, cm_surface::Error::RenderIntent as u32)
    );
    assert_eq!(
        error_of(|peer, cm| {
            let compositor: WlCompositor = peer.bind(6);
            let surface = compositor.create_surface(&peer.qh(), ());
            let cm_surface = cm.get_surface(&surface, &peer.qh(), ());
            surface.destroy();
            cm_surface.unset_image_description();
        }),
        expect(SURFACE, cm_surface::Error::Inert as u32)
    );
}

/// A committed 4×4 XRGB8888 SHM buffer on `surface`.
fn attach_rgb(peer: &Peer, surface: &WlSurface) {
    let shm: WlShm = peer.bind(1);
    let file = file_with(&[0xff; 64]);
    let pool = shm.create_pool(file.as_fd(), 64, &peer.qh(), ());
    let buffer = pool.create_buffer(0, 4, 4, 16, wl_shm::Format::Xrgb8888, &peer.qh(), ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 4, 4);
}

fn representation_error_of(
    request: impl FnOnce(&Peer, &WlSurface, &WpColorRepresentationManagerV1),
) -> Option<(String, u32)> {
    let (mut comp, mut manager, mut peer) = peer();
    let repr: WpColorRepresentationManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let surface = compositor.create_surface(&peer.qh(), ());
    pump(&mut comp, &mut manager, &mut peer);
    request(&peer, &surface, &repr);
    pump(&mut comp, &mut manager, &mut peer);
    peer.error()
}

#[test]
fn color_representation_follows_mutter() {
    const MANAGER: &str = "wp_color_representation_manager_v1";
    const SURFACE: &str = "wp_color_representation_surface_v1";

    // Straight alpha and identity coefficients on an RGB buffer are fine.
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            let rs = repr.get_surface(surface, &peer.qh(), ());
            rs.set_alpha_mode(repr::AlphaMode::Straight);
            rs.set_coefficients_and_range(repr::Coefficients::Identity, repr::Range::Limited);
            attach_rgb(peer, surface);
            surface.commit();
        }),
        None
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            repr.get_surface(surface, &peer.qh(), ());
            repr.get_surface(surface, &peer.qh(), ());
        }),
        expect(MANAGER, repr_manager::Error::SurfaceExists as u32)
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            repr.get_surface(surface, &peer.qh(), ())
                .set_alpha_mode(repr::AlphaMode::PremultipliedOptical);
        }),
        expect(SURFACE, repr::Error::AlphaMode as u32)
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            repr.get_surface(surface, &peer.qh(), ())
                .set_coefficients_and_range(repr::Coefficients::Fcc, repr::Range::Full);
        }),
        expect(SURFACE, repr::Error::Coefficients as u32)
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            let rs = repr.get_surface(surface, &peer.qh(), ());
            rs.set_coefficients_and_range(repr::Coefficients::Bt709, repr::Range::Limited);
            attach_rgb(peer, surface);
            surface.commit();
        }),
        expect(SURFACE, repr::Error::PixelFormat as u32)
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            let rs = repr.get_surface(surface, &peer.qh(), ());
            rs.set_chroma_location(repr::ChromaLocation::Type0);
            attach_rgb(peer, surface);
            surface.commit();
        }),
        expect(SURFACE, repr::Error::PixelFormat as u32)
    );
    assert_eq!(
        representation_error_of(|peer, surface, repr| {
            let rs = repr.get_surface(surface, &peer.qh(), ());
            surface.destroy();
            rs.set_alpha_mode(repr::AlphaMode::Straight);
        }),
        expect(SURFACE, repr::Error::Inert as u32)
    );
}

#[test]
fn synchronized_children_validate_representation_when_parent_applies_the_buffer() {
    for chroma in [false, true] {
        let (mut comp, mut manager, mut peer) = peer();
        let compositor: WlCompositor = peer.bind(6);
        let subcompositor: WlSubcompositor = peer.bind(1);
        let representation: WpColorRepresentationManagerV1 = peer.bind(1);
        let parent = compositor.create_surface(&peer.qh(), ());
        let child = compositor.create_surface(&peer.qh(), ());
        let _role = subcompositor.get_subsurface(&child, &parent, &peer.qh(), ());
        let color = representation.get_surface(&child, &peer.qh(), ());
        if chroma {
            color.set_chroma_location(repr::ChromaLocation::Type0);
        } else {
            color.set_coefficients_and_range(repr::Coefficients::Bt709, repr::Range::Limited);
        }
        attach_rgb(&peer, &child);
        child.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            peer.error(),
            None,
            "synchronized state has not taken effect"
        );
        parent.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            peer.error(),
            expect(
                "wp_color_representation_surface_v1",
                repr::Error::PixelFormat as u32
            )
        );
    }
}
