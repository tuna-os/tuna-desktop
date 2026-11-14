//! What the compositor and its shell agree on about OpenGL.
//!
//! GSK takes its GL renderer even on Mesa's software rasterizers (the GL
//! fallback precedes cairo in `gskrenderer.c`), which costs the shell an
//! LLVM shader compile per new shader variant and keeps Mesa, LLVM and
//! llvmpipe's rasterizer threads in the process for its lifetime. The
//! shell therefore draws with cairo whenever OpenGL is software. The
//! compositor knows that from its own context and says so in the shell's
//! environment ([`GSK_RENDERER_ENV`]), so the shell need not create a GL
//! context to find out.

/// GSK reads this once, for the first renderer it creates.
pub const GSK_RENDERER_ENV: &str = "GSK_RENDERER";

/// GSK's CPU renderer.
pub const GSK_RENDERER_CAIRO: &str = "cairo";

/// Whether a `GL_RENDERER` string names one of Mesa's software
/// rasterizers.
pub fn is_software_renderer(renderer: &str) -> bool {
    ["llvmpipe", "softpipe"]
        .iter()
        .any(|name| renderer.contains(name))
}

/// The GSK renderer a compositor whose own `GL_RENDERER` is `gl_renderer`
/// gives its shell: cairo on software GL, nothing (GSK's own choice) on a
/// GPU. `chosen` is whether the session already set [`GSK_RENDERER_ENV`],
/// which still decides.
pub fn shell_gsk_renderer(chosen: bool, gl_renderer: Option<&str>) -> Option<&'static str> {
    (!chosen && gl_renderer.is_some_and(is_software_renderer)).then_some(GSK_RENDERER_CAIRO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesa_software_rasterizers_are_software() {
        assert!(is_software_renderer("llvmpipe (LLVM 23.1.0, 256 bits)"));
        assert!(is_software_renderer("softpipe"));
        assert!(!is_software_renderer("virgl (AMD Radeon Graphics)"));
        assert!(!is_software_renderer(
            "Mesa Intel(R) UHD Graphics 620 (KBL GT2)"
        ));
        assert!(!is_software_renderer("NVIDIA GeForce RTX 4070/PCIe/SSE2"));
    }

    #[test]
    fn the_shell_gets_cairo_only_on_software_gl_and_only_when_unset() {
        let llvmpipe = Some("llvmpipe (LLVM 20.1.2, 256 bits)");
        assert_eq!(shell_gsk_renderer(false, llvmpipe), Some("cairo"));
        // A real GPU keeps GSK's own renderer.
        assert_eq!(
            shell_gsk_renderer(false, Some("Mesa Intel(R) UHD Graphics 620 (KBL GT2)")),
            None
        );
        // The session's own GSK_RENDERER (inherited by the shell) decides.
        assert_eq!(shell_gsk_renderer(true, llvmpipe), None);
        // No renderer string: the shell finds out for itself.
        assert_eq!(shell_gsk_renderer(false, None), None);
    }
}
