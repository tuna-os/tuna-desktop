//! File-descriptor sources on the main loop (`g_unix_fd_add_full`).
//!
//! glib-rs 0.22 moved `unix_fd_add_local` into the separate glib-unix
//! crate; the function itself is part of libglib, which every GTK
//! process links, so the shell binds it here. The shell wakes on its
//! sockets and timers this way instead of polling them.

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::os::raw::{c_int, c_uint};

use gtk4::glib;
use gtk4::glib::ffi::{gboolean, gpointer, GDestroyNotify, GIOCondition};
use gtk4::glib::translate::{from_glib, IntoGlib};

type UnixFdSourceFunc = Option<unsafe extern "C" fn(c_int, GIOCondition, gpointer) -> gboolean>;

extern "C" {
    fn g_unix_fd_add_full(
        priority: c_int,
        fd: c_int,
        condition: GIOCondition,
        function: UnixFdSourceFunc,
        user_data: gpointer,
        notify: GDestroyNotify,
    ) -> c_uint;
}

unsafe extern "C" fn trampoline<F>(fd: c_int, condition: GIOCondition, data: gpointer) -> gboolean
where
    F: FnMut(RawFd, glib::IOCondition) -> glib::ControlFlow + 'static,
{
    // SAFETY: `data` is the `RefCell<F>` boxed in `add_local`, alive
    // until `destroy` runs; GLib never re-enters a source's dispatch.
    let func = unsafe { &*(data as *const RefCell<F>) };
    let condition = unsafe { from_glib(condition) };
    (func.borrow_mut())(fd, condition).into_glib()
}

unsafe extern "C" fn destroy<F>(data: gpointer) {
    // SAFETY: GLib calls this once, after the last dispatch.
    drop(unsafe { Box::from_raw(data as *mut RefCell<F>) });
}

/// Call `func` on the main loop whenever `fd` meets `condition`, until
/// it returns [`glib::ControlFlow::Break`] or the source is removed.
/// `fd` must stay open while the source lives. Main thread only (the
/// closure is not `Send`).
pub fn add_local<F>(fd: RawFd, condition: glib::IOCondition, func: F) -> glib::SourceId
where
    F: FnMut(RawFd, glib::IOCondition) -> glib::ControlFlow + 'static,
{
    assert!(
        glib::MainContext::default().is_owner(),
        "fd_watch::add_local off the main thread"
    );
    let data = Box::into_raw(Box::new(RefCell::new(func))) as gpointer;
    // SAFETY: the trampoline and destroy notify agree on `F`; the
    // default context is owned by this thread, so `func` only ever runs
    // here.
    let id = unsafe {
        g_unix_fd_add_full(
            glib::ffi::G_PRIORITY_DEFAULT,
            fd,
            condition.into_glib(),
            Some(trampoline::<F>),
            data,
            Some(destroy::<F>),
        )
    };
    // SAFETY: a fresh, nonzero source id.
    unsafe { from_glib(id) }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_readable_socket_wakes_the_loop_and_break_removes_the_source() {
        let context = glib::MainContext::default();
        // Other tests may hold the default context for a moment.
        let deadline = Instant::now() + Duration::from_secs(10);
        let _owner = loop {
            match context.acquire() {
                Ok(owner) => break owner,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("default main context: {e}"),
            }
        };
        let (mut ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let seen = Rc::new(Cell::new(0));
        let counted = seen.clone();
        add_local(
            peer.as_raw_fd(),
            glib::IOCondition::IN,
            move |_, condition| {
                assert!(condition.contains(glib::IOCondition::IN));
                counted.set(counted.get() + 1);
                glib::ControlFlow::Break
            },
        );
        // Nothing to read: the source stays quiet.
        while context.iteration(false) {}
        assert_eq!(seen.get(), 0);
        ours.write_all(b"x").expect("write");
        while seen.get() == 0 && Instant::now() < deadline {
            context.iteration(false);
        }
        assert_eq!(seen.get(), 1);
        // Still readable, but the source is gone.
        while context.iteration(false) {}
        assert_eq!(seen.get(), 1);
    }
}
