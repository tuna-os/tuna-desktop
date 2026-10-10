//! Real xdg-dialog modal + SHM pixel peer for the isolated dim rendering proof.
use std::{
    io::{Seek, SeekFrom, Write},
    os::fd::AsFd,
    time::Duration,
};
use wayland_client::{
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::{
    dialog::v1::client::{xdg_dialog_v1, xdg_wm_dialog_v1},
    shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base},
};
struct Window {
    surface: wl_surface::WlSurface,
    xdg: xdg_surface::XdgSurface,
    top: xdg_toplevel::XdgToplevel,
    buffer: wl_buffer::WlBuffer,
    dialog: Option<xdg_dialog_v1::XdgDialogV1>,
    _file: std::fs::File,
    size: (i32, i32),
}
#[derive(Default)]
struct Peer {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    base: Option<xdg_wm_base::XdgWmBase>,
    dialogs: Option<xdg_wm_dialog_v1::XdgWmDialogV1>,
    windows: Vec<Option<Window>>,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        s: &mut Self,
        p: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(p.bind(name, version.min(4), q, ())),
                "wl_shm" => s.shm = Some(p.bind(name, 1, q, ())),
                "xdg_wm_base" => s.base = Some(p.bind(name, version.min(6), q, ())),
                "xdg_wm_dialog_v1" => s.dialogs = Some(p.bind(name, 1, q, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
    fn event(
        _: &mut Self,
        p: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            p.pong(serial);
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, usize> for Peer {
    fn event(
        s: &mut Self,
        p: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            p.ack_configure(serial);
            if let Some(Some(w)) = s.windows.get(*index) {
                w.surface.attach(Some(&w.buffer), 0, 0);
                w.surface.damage_buffer(0, 0, w.size.0, w.size.1);
                w.surface.commit();
            }
        }
    }
}
wayland_client::delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Peer: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Peer: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Peer: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);
wayland_client::delegate_noop!(Peer: ignore xdg_wm_dialog_v1::XdgWmDialogV1);
wayland_client::delegate_noop!(Peer: ignore xdg_dialog_v1::XdgDialogV1);
impl Peer {
    fn create(&mut self, q: &QueueHandle<Self>, index: usize, modal: bool) {
        let parent = index == 0;
        let fullscreen = parent && std::env::var_os("TUNA_PROOF_FULLSCREEN").is_some();
        let (width, height) = if fullscreen {
            (1280, 800)
        } else if parent {
            (500, 350)
        } else {
            (220, 100)
        };
        let mut file = tempfile::tempfile().unwrap();
        let mut bytes = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                // A fully transparent strip catches RGB-only shader/alpha regressions.
                let color: u32 = if parent && x < 40 {
                    0
                } else if !parent {
                    0xff4e9a06
                } else if y < height / 2 {
                    0xffef2929
                } else {
                    0xff3465a4
                };
                bytes.extend_from_slice(&color.to_ne_bytes());
            }
        }
        file.write_all(&bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let pool = self
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), bytes.len() as i32, q, ());
        let buffer =
            pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, q, ());
        pool.destroy();
        let surface = self.compositor.as_ref().unwrap().create_surface(q, ());
        let xdg = self
            .base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, q, index);
        let top = xdg.get_toplevel(q, ());
        top.set_app_id("tuna-modal-dim-proof".into());
        top.set_title(format!("Modal dim {index}"));
        if fullscreen {
            top.set_fullscreen(None);
        }
        if !parent {
            top.set_parent(Some(&self.windows[0].as_ref().unwrap().top));
        }
        let dialog = if !parent {
            let d = self
                .dialogs
                .as_ref()
                .expect("xdg-dialog required")
                .get_xdg_dialog(&top, q, ());
            if modal {
                d.set_modal();
            }
            Some(d)
        } else {
            None
        };
        xdg.set_window_geometry(0, 0, width, height);
        while self.windows.len() <= index {
            self.windows.push(None);
        }
        self.windows[index] = Some(Window {
            surface,
            xdg,
            top,
            buffer,
            dialog,
            _file: file,
            size: (width, height),
        });
        self.windows[index].as_ref().unwrap().surface.commit();
    }
    fn close(&mut self, index: usize) {
        if let Some(w) = self.windows.get_mut(index).and_then(Option::take) {
            if let Some(d) = w.dialog {
                d.destroy();
            }
            w.top.destroy();
            w.xdg.destroy();
            w.surface.destroy();
            w.buffer.destroy();
        }
    }
}
fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let mut peer = Peer::default();
    conn.display().get_registry(&q, ());
    queue.roundtrip(&mut peer).unwrap();
    peer.create(&q, 0, false);
    queue.roundtrip(&mut peer).unwrap();
    let path = std::env::var("TUNA_LIFECYCLE_COMMAND").unwrap();
    let mut generation = None;
    loop {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                let next = value["generation"].as_u64();
                if next != generation {
                    generation = next;
                    match value["action"].as_str().unwrap_or("") {
                        "minimize" => peer.windows[0].as_ref().unwrap().top.set_minimized(),
                        "new" => {
                            peer.close(0);
                            peer.create(&q, 0, false);
                        }
                        "dialog" => peer.create(&q, 1, true),
                        "second-dialog" => peer.create(&q, 2, true),
                        "nonmodal" => peer.create(&q, 1, false),
                        "set-modal" => peer.windows[1]
                            .as_ref()
                            .unwrap()
                            .dialog
                            .as_ref()
                            .unwrap()
                            .set_modal(),
                        "unset-modal" => peer.windows[1]
                            .as_ref()
                            .unwrap()
                            .dialog
                            .as_ref()
                            .unwrap()
                            .unset_modal(),
                        "close-dialog" => peer.close(1),
                        "close-second" => peer.close(2),
                        "close" => {
                            for i in (0..peer.windows.len()).rev() {
                                peer.close(i);
                            }
                            conn.flush().unwrap();
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
        queue.roundtrip(&mut peer).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
}
