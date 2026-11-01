//! Native consumer: seat-repeat-client <9|10> [seconds] [expected-text].
//! Map a real text-input-v3 field, drive it with native keys/IBus, and retain
//! JSON receipts. Version 9 implements client repeat; version 10 only consumes
//! Repeated. An expected-text argument makes a missing IBus commit fail.
use std::{collections::{HashMap, HashSet}, io::{Read, Write}, os::fd::{AsFd, AsRawFd}, time::{Duration, Instant}};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, protocol::{
    wl_registry::{self, WlRegistry}, wl_compositor::WlCompositor,
    wl_surface::WlSurface, wl_seat::WlSeat, wl_keyboard::{self, WlKeyboard, KeyState},
    wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer,
}};
use wayland_protocols::{xdg::shell::client::{xdg_wm_base::{self, XdgWmBase},
    xdg_surface::{self, XdgSurface}, xdg_toplevel::XdgToplevel},
    wp::text_input::zv3::client::{zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{self, ZwpTextInputV3}}};
use xkbcommon::xkb;

struct Client {
    globals: HashMap<String, (u32, u32)>,
    version: u32,
    xkb: Option<(xkb::Keymap, xkb::State)>,
    repeat: Option<(u32, Instant)>,
    held: HashSet<u32>,
    rate: i32,
    delay: i32,
    focused: bool,
    text: String,
    pending_commit: Option<String>,
    pending_preedit: Option<String>,
    wire_repeats: usize,
    client_repeats: usize,
    commits: usize,
}
impl Client {
    fn bind<I: Proxy + 'static>(&self, registry: &WlRegistry, qh: &QueueHandle<Self>, version: u32) -> I
    where Self: Dispatch<I, ()> {
        let (name, offered) = self.globals[I::interface().name];
        assert!(offered >= version, "{} needs {version}, offered {offered}", I::interface().name);
        registry.bind(name, version, qh, ())
    }
    fn typed(&mut self, key: u32, source: &str) {
        let Some((_, state)) = self.xkb.as_ref() else { return };
        let sym = state.key_get_one_sym((key + 8).into()).raw();
        let text = state.key_get_utf8((key + 8).into());
        if sym == 0xff08 { self.text.pop(); }
        else if !text.chars().any(char::is_control) { self.text.push_str(&text); }
        println!("{}", serde_json::json!({"event":"typed", "source":source,
            "version":self.version, "key":key, "text":self.text}));
    }
    fn tick(&mut self) {
        let Some((key, due)) = self.repeat else { return };
        if !self.focused || self.rate <= 0 || self.version >= 10 { self.repeat = None; return; }
        if Instant::now() < due { return; }
        self.client_repeats += 1;
        self.typed(key, "client-repeat");
        self.repeat = Some((key, Instant::now() + Duration::from_secs_f64(1.0 / f64::from(self.rate))));
    }
}
impl Dispatch<WlRegistry, ()> for Client {
    fn event(state: &mut Self, _: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event { state.globals.insert(interface, (name, version)); }
    }
}
impl Dispatch<WlKeyboard, ()> for Client {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                state.repeat = None;
                let mut text = String::new();
                std::fs::File::from(fd).take(u64::from(size)).read_to_string(&mut text).unwrap();
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                let keymap = xkb::Keymap::new_from_string(&context, text.trim_end_matches('\0').to_owned(),
                    xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS).expect("valid compositor keymap");
                let xkb_state = xkb::State::new(&keymap);
                state.xkb = Some((keymap, xkb_state));
                println!("{}", serde_json::json!({"event":"keymap", "version":state.version}));
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                assert!(rate >= 0 && delay >= 0);
                if state.version >= 10 { assert_eq!((rate, delay), (0, 0)); }
                state.rate = rate; state.delay = delay; state.repeat = None;
                println!("{}", serde_json::json!({"event":"repeat-info", "version":state.version, "rate":rate, "delay":delay}));
            }
            wl_keyboard::Event::Enter { keys, .. } => {
                state.focused = true;
                state.held.clear();
                for bytes in keys.chunks_exact(4) {
                    let key = u32::from_ne_bytes(bytes.try_into().unwrap());
                    state.held.insert(key);
                    if let Some((_, xkb)) = state.xkb.as_mut() { xkb.update_key((key + 8).into(), xkb::KeyDirection::Down); }
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                state.focused = false; state.repeat = None; state.held.clear();
                if let Some((keymap, xkb)) = state.xkb.as_mut() { *xkb = xkb::State::new(keymap); }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some((_, xkb)) = state.xkb.as_mut() { xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group); }
            }
            wl_keyboard::Event::Key { serial, time, key, state: WEnum::Value(key_state) } => {
                println!("{}", serde_json::json!({"event":"key", "version":state.version,
                    "serial":serial, "time":time, "key":key, "state":u32::from(key_state)}));
                match key_state {
                    KeyState::Pressed => {
                        assert!(state.held.insert(key), "duplicate original press");
                        state.typed(key, "press");
                        if let Some((keymap, xkb)) = state.xkb.as_mut() {
                            xkb.update_key((key + 8).into(), xkb::KeyDirection::Down);
                            if state.version < 10 && state.rate > 0 && keymap.key_repeats((key + 8).into()) {
                                state.repeat = Some((key, Instant::now() + Duration::from_millis(state.delay as u64)));
                            }
                        }
                    }
                    KeyState::Released => {
                        state.held.remove(&key);
                        if state.repeat.is_some_and(|(held, _)| held == key) { state.repeat = None; }
                        if let Some((_, xkb)) = state.xkb.as_mut() { xkb.update_key((key + 8).into(), xkb::KeyDirection::Up); }
                    }
                    KeyState::Repeated => {
                        assert!(state.version >= 10, "Repeated leaked to a legacy keyboard");
                        assert!(state.focused && state.held.contains(&key), "repeat without an admitted held press");
                        state.wire_repeats += 1;
                        state.typed(key, "compositor-repeat");
                    }
                    _ => panic!("unknown key state"),
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<XdgWmBase, ()> for Client {
    fn event(_: &mut Self, wm: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event { wm.pong(serial); }
    }
}
impl Dispatch<XdgSurface, ()> for Client {
    fn event(_: &mut Self, surface: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event { surface.ack_configure(serial); }
    }
}
impl Dispatch<ZwpTextInputV3, ()> for Client {
    fn event(state: &mut Self, field: &ZwpTextInputV3, event: zwp_text_input_v3::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                field.enable();
                field.set_content_type(zwp_text_input_v3::ContentHint::None, zwp_text_input_v3::ContentPurpose::Normal);
                field.set_cursor_rectangle(20, 30, 1, 24);
                field.set_surrounding_text(state.text.clone(), state.text.len() as i32, state.text.len() as i32);
                field.commit();
            }
            zwp_text_input_v3::Event::Leave { .. } => { field.disable(); field.commit(); }
            zwp_text_input_v3::Event::CommitString { text } => { state.pending_commit = text; }
            zwp_text_input_v3::Event::PreeditString { text, .. } => { state.pending_preedit = text; }
            zwp_text_input_v3::Event::Done { serial } => {
                let before = state.text.len();
                if let Some(text) = state.pending_commit.take() { state.commits += 1; state.text.push_str(&text); }
                println!("{}", serde_json::json!({"event":"text-input-done", "serial":serial,
                    "version":state.version, "text":state.text, "preedit":state.pending_preedit.take(), "commits":state.commits}));
                if state.text.len() != before {
                    field.set_surrounding_text(state.text.clone(), state.text.len() as i32, state.text.len() as i32);
                    field.commit();
                }
            }
            zwp_text_input_v3::Event::DeleteSurroundingText { before_length, after_length } => {
                assert_eq!(after_length, 0, "proof field cursor is at the end");
                let len = state.text.len().checked_sub(before_length as usize).expect("bounded deletion");
                assert!(state.text.is_char_boundary(len)); state.text.truncate(len);
            }
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlSeat);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore ZwpTextInputManagerV3);

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let version: u32 = args.get(1).expect("seat version 9 or 10").parse().unwrap();
    assert!(version == 9 || version == 10);
    let seconds: u64 = args.get(2).map_or(30, |n| n.parse().unwrap());
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue(); let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = Client { globals: HashMap::new(), version, xkb: None, repeat: None, held: HashSet::new(),
        rate: 0, delay: 0, focused: false, text: String::new(), pending_commit: None,
        pending_preedit: None, wire_repeats: 0, client_repeats: 0, commits: 0 };
    queue.roundtrip(&mut client).unwrap();
    let compositor: WlCompositor = client.bind(&registry, &qh, 6);
    let wm: XdgWmBase = client.bind(&registry, &qh, 6);
    let shm: WlShm = client.bind(&registry, &qh, 1);
    let seat: WlSeat = client.bind(&registry, &qh, version);
    let _keyboard = seat.get_keyboard(&qh, ());
    let manager: ZwpTextInputManagerV3 = client.bind(&registry, &qh, 1);
    let _field = manager.get_text_input(&seat, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_app_id(format!("org.tuna.SeatRepeatProof.v{version}"));
    top.set_title(format!("Seat {version} repeat and IBus proof"));
    surface.commit(); queue.roundtrip(&mut client).unwrap();
    let mut file = tempfile::tempfile().unwrap(); file.write_all(&vec![0xa0; 400 * 160 * 4]).unwrap();
    let pool = shm.create_pool(file.as_fd(), 400 * 160 * 4, &qh, ());
    let buffer = pool.create_buffer(0, 400, 160, 400 * 4, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0); surface.damage(0, 0, 400, 160); surface.commit();
    queue.roundtrip(&mut client).unwrap();
    println!("{}", serde_json::json!({"event":"ready", "version":version}));
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until {
        queue.dispatch_pending(&mut client).unwrap(); queue.flush().unwrap(); client.tick();
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd { fd: conn.as_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let result = unsafe { libc::poll(&mut fd, 1, 5) };
            if result > 0 { guard.read().unwrap(); }
            else if result < 0 { assert_eq!(std::io::Error::last_os_error().kind(), std::io::ErrorKind::Interrupted); }
        }
        std::io::stdout().flush().unwrap();
    }
    println!("{}", serde_json::json!({"event":"summary", "version":version, "text":client.text,
        "wire_repeats":client.wire_repeats, "client_repeats":client.client_repeats, "commits":client.commits}));
    if let Some(expected) = args.get(3) {
        assert!(client.commits > 0 && client.text.contains(expected), "missing real input-method commit: {expected}");
    }
}
