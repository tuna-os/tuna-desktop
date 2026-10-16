//! The shell's GSK renderer under software OpenGL.
//!
//! GSK declines its GL renderer when OpenGL is llvmpipe ("Not using GL:
//! renderer is llvmpipe"), but without a hardware Vulkan device its
//! fallback chain takes the same llvmpipe GL renderer anyway
//! (`gskrenderer.c`: the GL fallback precedes cairo). On llvmpipe every
//! new shader variant is compiled through LLVM, and the shell's
//! surfaces keep producing new ones: on the Marlin benchmark VM the
//! shell spent about 4 CPU-seconds on three overview searches, and a
//! local replica put three quarters of the first one in LLVM. The
//! cairo renderer draws the same nodes on the CPU without a compiler,
//! so the shell takes it whenever OpenGL is a software rasterizer.
//! `GSK_RENDERER`, when set, still decides.

use std::ffi::CStr;

use gtk4 as gtk;
use gtk4::prelude::*;

/// Choose the cairo renderer when OpenGL on `display` is a software
/// rasterizer. Call before the first window is realized: GSK reads
/// `GSK_RENDERER` once, for the first renderer it creates.
pub fn prefer_cairo_on_software_gl(display: &gtk::gdk::Display) {
    if std::env::var_os("GSK_RENDERER").is_some() {
        return;
    }
    let Some(renderer) = gl_renderer(display) else {
        return;
    };
    if is_software(&renderer) {
        eprintln!("tuna-shell-gtk: OpenGL is {renderer}; drawing with cairo");
        std::env::set_var("GSK_RENDERER", "cairo");
    }
}

/// Whether a `GL_RENDERER` string names one of Mesa's software
/// rasterizers.
pub fn is_software(renderer: &str) -> bool {
    ["llvmpipe", "softpipe"]
        .iter()
        .any(|name| renderer.contains(name))
}

/// `GL_RENDERER` of a GL context on `display`, as GSK's own check reads
/// it; `None` without OpenGL.
fn gl_renderer(display: &gtk::gdk::Display) -> Option<String> {
    display.prepare_gl().ok()?;
    let context = display.create_gl_context().ok()?;
    context.realize().ok()?;
    context.make_current();
    let renderer = gl_get_renderer();
    gtk::gdk::GLContext::clear_current();
    renderer
}

/// `glGetString(GL_RENDERER)` for the current context, through the
/// libepoxy dispatch GDK itself links (no GL loader of our own).
fn gl_get_renderer() -> Option<String> {
    const GL_RENDERER: u32 = 0x1F01;
    type GetString = unsafe extern "C" fn(u32) -> *const libc::c_char;
    // SAFETY: libepoxy exports `epoxy_glGetString` as a variable holding
    // the dispatch function (`PFNGLGETSTRINGPROC`, never null once the
    // library is loaded); a missing symbol yields null and `None`. The
    // returned string is static, NUL-terminated GL state, and a context
    // is current.
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"epoxy_glGetString".as_ptr());
        if symbol.is_null() {
            return None;
        }
        let get = (*symbol.cast::<Option<GetString>>())?;
        let text = get(GL_RENDERER);
        (!text.is_null()).then(|| CStr::from_ptr(text).to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::is_software;

    #[test]
    fn mesa_software_rasterizers_are_software() {
        assert!(is_software("llvmpipe (LLVM 23.1.0, 256 bits)"));
        assert!(is_software("softpipe"));
        assert!(!is_software("virgl (AMD Radeon Graphics)"));
        assert!(!is_software("Mesa Intel(R) UHD Graphics 620 (KBL GT2)"));
        assert!(!is_software("NVIDIA GeForce RTX 4070/PCIe/SSE2"));
    }
}
