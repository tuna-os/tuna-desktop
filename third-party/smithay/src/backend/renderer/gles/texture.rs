use ffi::Gles2;

use super::*;
use std::sync::Arc;

/// A handle to a GLES texture
///
/// The texture can be used with the same [`GlesRenderer`] it was created with, or one using a
/// shared [`EGLContext`].
#[derive(Debug, Clone)]
pub struct GlesTexture(pub(super) Arc<GlesTextureInternal>);

impl GlesTexture {
    /// Create a GlesTexture from a raw gl texture id.
    ///
    /// It is expected to not be external or y_inverted.
    ///
    /// Ownership over the texture is taken by the renderer, you should not free the texture yourself.
    ///
    /// # Safety
    ///
    /// The renderer cannot make sure `tex` is a valid texture id.
    pub unsafe fn from_raw(
        renderer: &GlesRenderer,
        internal_format: Option<ffi::types::GLenum>,
        opaque: bool,
        tex: ffi::types::GLuint,
        size: Size<i32, BufferCoord>,
    ) -> GlesTexture {
        GlesTexture(Arc::new(GlesTextureInternal {
            texture: tex,
            sync: RwLock::default(),
            read_sync: ReadSync::default(),
            format: internal_format,
            has_alpha: !opaque,
            is_external: false,
            y_inverted: false,
            size,
            egl_images: None,
            destruction_callback_sender: renderer.destruction_callback_sender.clone(),
        }))
    }

    /// OpenGL texture id of this texture
    ///
    /// This id will become invalid, when the GlesTexture is dropped and does not transfer ownership.
    pub fn tex_id(&self) -> ffi::types::GLuint {
        self.0.texture
    }

    /// Whether the texture is upside down
    pub fn is_y_inverted(&self) -> bool {
        self.0.y_inverted
    }

    /// Whether this is the only reference to this texture (strong or weak)
    ///
    /// Note that this tracks only references to this Smithay object (that you can get by cloning
    /// it). If you make a reference via OpenGL directly somehow, you need to keep track of it on
    /// your own.
    pub fn is_unique_reference(&mut self) -> bool {
        Arc::get_mut(&mut self.0).is_some()
    }
}

/// A GL fence ordering later writes to textures after the reads it covers.
///
/// A [`GlesFrame`] reading textures through an unshared context creates one fence when it finishes
/// and hands it to every texture it read, so the fence is reference counted. The last holder
/// queues its deletion on the renderer that created it.
#[derive(Debug)]
pub(super) struct ReadFence {
    sync: ffi::types::GLsync,
    destruction_callback_sender: Sender<CleanupResource>,
}

impl ReadFence {
    /// Inserts a fence after every command issued so far in the current context.
    pub(super) fn new(gl: &Gles2, destruction_callback_sender: Sender<CleanupResource>) -> Arc<Self> {
        Arc::new(ReadFence {
            sync: unsafe { gl.FenceSync(ffi::SYNC_GPU_COMMANDS_COMPLETE, 0) },
            destruction_callback_sender,
        })
    }

    /// Makes the current context wait for this fence, so that a fence created next in this
    /// context covers the reads this one covers, whichever context issued them.
    fn wait(&self, gl: &Gles2) {
        unsafe { gl.WaitSync(self.sync, 0, ffi::TIMEOUT_IGNORED) };
    }
}

impl Drop for ReadFence {
    fn drop(&mut self) {
        let _ = self
            .destruction_callback_sender
            .send(CleanupResource::Sync(self.sync as *const _));
    }
}

/// The read fence of a texture, covering every read issued so far.
///
/// A draw through a shared context fences its read while it holds the texture's read lock, so a
/// writer taking the write lock afterwards waits for it. A frame drawing through an unshared
/// context fences all its reads once, when it finishes: no other context can reach those
/// textures in the meantime. It is not behind the [`TextureSync`] lock because, after a leaked
/// frame, the next frame's target may hold the write lock of a texture that frame fences.
#[derive(Debug, Default)]
pub(super) struct ReadSync(Mutex<Option<Arc<ReadFence>>>);

impl ReadSync {
    /// Fences the reads issued so far in the current context, covering earlier ones too.
    pub(super) fn update(&self, gl: &Gles2, destruction_callback_sender: Sender<CleanupResource>) {
        let mut read_sync = self.0.lock().unwrap();
        if let Some(old) = read_sync.take() {
            old.wait(gl);
        }
        *read_sync = Some(ReadFence::new(gl, destruction_callback_sender));
    }

    /// Makes the current context wait for the current fence, returning it.
    ///
    /// Pass the result to [`ReadSync::replace`] with a fence created next in this context.
    pub(super) fn wait_to_replace(&self, gl: &Gles2) -> Option<Arc<ReadFence>> {
        let read_sync = self.0.lock().unwrap();
        if let Some(old) = &*read_sync {
            old.wait(gl);
        }
        read_sync.clone()
    }

    /// Replaces the fence `waited` by `fence`, created after [`ReadSync::wait_to_replace`]
    /// returned `waited` in the same context.
    ///
    /// If another context fenced its reads in between, `fence` does not cover them: this
    /// waits for that fence too and fences again.
    pub(super) fn replace(
        &self,
        gl: &Gles2,
        waited: Option<Arc<ReadFence>>,
        fence: &Arc<ReadFence>,
        destruction_callback_sender: &Sender<CleanupResource>,
    ) {
        let mut read_sync = self.0.lock().unwrap();
        let unchanged = match (&*read_sync, &waited) {
            (None, None) => true,
            (Some(current), Some(waited)) => Arc::ptr_eq(current, waited),
            _ => false,
        };
        if unchanged {
            *read_sync = Some(fence.clone());
        } else {
            if let Some(current) = &*read_sync {
                current.wait(gl);
            }
            fence.wait(gl);
            *read_sync = Some(ReadFence::new(gl, destruction_callback_sender.clone()));
        }
    }

    /// Orders the current context after every read; for writers holding the write lock.
    pub(super) fn wait_for_reads(&self, gl: &Gles2) {
        let mut read_sync = self.0.lock().unwrap();
        if let Some(fence) = &*read_sync {
            match unsafe { gl.ClientWaitSync(fence.sync, 0, 0) } {
                ffi::ALREADY_SIGNALED | ffi::CONDITION_SATISFIED => {
                    let _ = read_sync.take();
                }
                _ => fence.wait(gl),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn current(&self) -> Option<Arc<ReadFence>> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Debug, Default)]
pub(super) struct TextureSync {
    write_sync: Mutex<Option<ffi::types::GLsync>>,
}

unsafe fn wait_for_syncpoint(sync: &mut Option<ffi::types::GLsync>, gl: &Gles2) {
    if let Some(sync_obj) = *sync {
        match gl.ClientWaitSync(sync_obj, 0, 0) {
            ffi::ALREADY_SIGNALED | ffi::CONDITION_SATISFIED => {
                let _ = sync.take();
                gl.DeleteSync(sync_obj);
            }
            _ => {
                gl.WaitSync(sync_obj, 0, ffi::TIMEOUT_IGNORED);
            }
        };
    }
}

impl TextureSync {
    pub(super) fn wait_for_upload(&self, gl: &Gles2) {
        unsafe {
            wait_for_syncpoint(&mut self.write_sync.lock().unwrap(), gl);
        }
    }

    pub(super) fn wait_for_all(&mut self, gl: &Gles2, read_sync: &ReadSync) {
        read_sync.wait_for_reads(gl);
        unsafe {
            wait_for_syncpoint(self.write_sync.get_mut().unwrap(), gl);
        }
    }

    pub(super) fn update_write(&mut self, gl: &Gles2) {
        let write_sync = self.write_sync.get_mut().unwrap();
        if let Some(old) = write_sync.take() {
            unsafe {
                gl.WaitSync(old, 0, ffi::TIMEOUT_IGNORED);
                gl.DeleteSync(old);
            };
        }

        *write_sync = Some(unsafe { gl.FenceSync(ffi::SYNC_GPU_COMMANDS_COMPLETE, 0) });
    }
}

#[derive(Debug)]
pub(super) struct GlesTextureInternal {
    pub(super) texture: ffi::types::GLuint,
    pub(super) sync: RwLock<TextureSync>,
    pub(super) read_sync: ReadSync,
    pub(super) format: Option<ffi::types::GLenum>,
    pub(super) has_alpha: bool,
    pub(super) is_external: bool,
    pub(super) y_inverted: bool,
    pub(super) size: Size<i32, BufferCoord>,
    pub(super) egl_images: Option<Vec<EGLImage>>,
    pub(super) destruction_callback_sender: Sender<CleanupResource>,
}
unsafe impl Send for GlesTextureInternal {}
unsafe impl Sync for GlesTextureInternal {}

impl Drop for GlesTextureInternal {
    fn drop(&mut self) {
        let _ = self
            .destruction_callback_sender
            .send(CleanupResource::Texture(self.texture));
        let mut sync = self.sync.write().unwrap();
        if let Some(sync) = sync.write_sync.get_mut().unwrap().take() {
            let _ = self
                .destruction_callback_sender
                .send(CleanupResource::Sync(sync as *const _));
        }
        if let Some(images) = self.egl_images.take() {
            for image in images {
                let _ = self
                    .destruction_callback_sender
                    .send(CleanupResource::EGLImage(image));
            }
        }
    }
}

impl Texture for GlesTexture {
    fn width(&self) -> u32 {
        self.0.size.w as u32
    }
    fn height(&self) -> u32 {
        self.0.size.h as u32
    }
    fn size(&self) -> Size<i32, BufferCoord> {
        self.0.size
    }
    fn format(&self) -> Option<Fourcc> {
        let fmt = gl_internal_format_to_fourcc(self.0.format?);
        if self.0.has_alpha {
            fmt
        } else {
            fmt.and_then(get_opaque)
        }
    }
}

/// Texture mapping of a GLES2 texture
#[derive(Debug)]
pub struct GlesMapping {
    pub(super) pbo: ffi::types::GLuint,
    pub(super) format: ffi::types::GLenum,
    pub(super) layout: ffi::types::GLenum,
    pub(super) has_alpha: bool,
    pub(super) size: Size<i32, BufferCoord>,
    pub(super) mapping: AtomicPtr<std::ffi::c_void>,
    pub(super) destruction_callback_sender: Sender<CleanupResource>,
}

impl Texture for GlesMapping {
    fn width(&self) -> u32 {
        self.size.w as u32
    }
    fn height(&self) -> u32 {
        self.size.h as u32
    }
    fn size(&self) -> Size<i32, BufferCoord> {
        self.size
    }
    fn format(&self) -> Option<Fourcc> {
        let fmt = gl_read_format_to_fourcc(self.format, self.layout);
        if self.has_alpha {
            fmt
        } else {
            fmt.and_then(get_opaque)
        }
    }
}
impl TextureMapping for GlesMapping {
    fn flipped(&self) -> bool {
        true
    }
    fn format(&self) -> Fourcc {
        Texture::format(self).expect("Should never happen")
    }
}

impl Drop for GlesMapping {
    fn drop(&mut self) {
        let _ = self.destruction_callback_sender.send(CleanupResource::Mapping(
            self.pbo,
            self.mapping.load(Ordering::SeqCst),
        ));
    }
}
