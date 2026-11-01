//! PipeWire screen-cast streams (#61), adapted from niri's
//! `src/screencasting/pw_utils.rs` (GPL-3.0-or-later, like Tuna Desktop).
//!
//! Monitor and window streams over shared memory (PipeWire allocates
//! and maps the memfd buffers), full frames at most every
//! [`FRAME_INTERVAL`]. SDR BT.709/gamma 2.2 BGRx is always the default.
//! An admitted HDR monitor additionally offers ten-bit BT.2020/PQ.
//! Window/area streams remain SDR, as in Mutter 51. dmabuf zero-copy,
//! damage tracking and cursor metadata are not implemented here.

use std::cell::RefCell;
use std::os::fd::{AsFd, BorrowedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use pipewire::context::ContextRc;
use pipewire::core::CoreRc;
use pipewire::loop_::Timeout;
use pipewire::main_loop::MainLoopRc;
use pipewire::properties::PropertiesBox;
use pipewire::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pipewire::spa::param::format_utils::parse_format;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, ChoiceValue, Pod, Property};
use pipewire::spa::sys::*;
use pipewire::spa::utils::{
    Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Rectangle, SpaTypes,
};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};

use crate::mutter::CastTarget;

/// Longest gap a client waits between frames, and the shortest we send.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const BYTES_PER_PIXEL: i32 = 4;

// Stable SPA ABI value from spa/param/video/color.h. Older build headers
// predate this enumerator; mandatory HDR properties prevent an unaware
// consumer from silently treating it as an SDR/default transfer function.
const TRANSFER_SMPTE2084: u32 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFormat {
    Sdr,
    Hdr10,
}

impl CaptureFormat {
    pub fn fourcc(self) -> smithay::backend::allocator::Fourcc {
        use smithay::backend::allocator::Fourcc;
        match self {
            Self::Sdr => Fourcc::Xrgb8888,
            Self::Hdr10 => Fourcc::Abgr2101010,
        }
    }

    fn from_negotiated(info: VideoInfoRaw, hdr_available: bool) -> Option<Self> {
        let range = info.color_range();
        let matrix = info.color_matrix();
        if !matches!(
            range,
            SPA_VIDEO_COLOR_RANGE_UNKNOWN | SPA_VIDEO_COLOR_RANGE_0_255
        ) || !matches!(
            matrix,
            SPA_VIDEO_COLOR_MATRIX_UNKNOWN | SPA_VIDEO_COLOR_MATRIX_RGB
        ) {
            return None;
        }
        if info.format() == VideoFormat::BGRx
            && matches!(
                info.color_primaries(),
                SPA_VIDEO_COLOR_PRIMARIES_UNKNOWN | SPA_VIDEO_COLOR_PRIMARIES_BT709
            )
            && matches!(
                info.transfer_function(),
                SPA_VIDEO_TRANSFER_UNKNOWN | SPA_VIDEO_TRANSFER_GAMMA22
            )
        {
            Some(Self::Sdr)
        } else if hdr_available
            && info.format() == VideoFormat::xBGR_210LE
            && info.color_primaries() == SPA_VIDEO_COLOR_PRIMARIES_BT2020
            && info.transfer_function() == TRANSFER_SMPTE2084
        {
            Some(Self::Hdr10)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CaptureFrame {
    pub size: (i32, i32),
    pub format: CaptureFormat,
}

/// Qualified, cached capture targets: FP16 extended-sRGB composition,
/// followed by BT.2020/PQ encoding into the actual ten-bit readback target.
#[cfg(feature = "drm")]
pub(crate) struct HdrCapture {
    pub(crate) scene: crate::color_output::HdrBuffer,
    pub(crate) output: smithay::backend::renderer::gles::GlesTexture,
}

#[cfg(feature = "drm")]
impl HdrCapture {
    pub(crate) fn new(
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) -> Result<Self, String> {
        use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer};
        let scene = crate::color_output::HdrBuffer::new(renderer, size)?;
        let mut output = renderer
            .create_buffer(CaptureFormat::Hdr10.fourcc(), (size.w, size.h).into())
            .map_err(|error| error.to_string())?;
        {
            let mut target = renderer
                .bind(&mut output)
                .map_err(|error| error.to_string())?;
            let mut frame = renderer
                .render(&mut target, size, smithay::utils::Transform::Normal)
                .map_err(|error| error.to_string())?;
            scene
                .present(&mut frame)
                .map_err(|error| error.to_string())?;
            frame
                .finish()
                .map_err(|error| error.to_string())?
                .wait()
                .map_err(|_| "HDR capture readback probe was interrupted".to_owned())?;
            // Scanout support alone does not establish ten-bit readback.
            // Exercise the exact boundary before offering HDR to a client.
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    smithay::utils::Rectangle::from_size((size.w, size.h).into()),
                    CaptureFormat::Hdr10.fourcc(),
                )
                .map_err(|error| error.to_string())?;
            renderer
                .map_texture(&mapping)
                .map_err(|error| error.to_string())?;
        }
        Ok(Self { scene, output })
    }
}

/// The PipeWire connection, its loop driven by the compositor's.
pub struct PipeWire {
    _context: ContextRc,
    core: CoreRc,
}

impl PipeWire {
    /// Connect to the session's PipeWire; `None` when it is not running.
    pub fn new<D: 'static>(event_loop: &LoopHandle<'static, D>) -> Option<Self> {
        pipewire::init();
        let main_loop = MainLoopRc::new(None).ok()?;
        let context = ContextRc::new(&main_loop, None).ok()?;
        let core = context.connect_rc(None).ok()?;
        struct LoopFd(MainLoopRc);
        impl AsFd for LoopFd {
            fn as_fd(&self) -> BorrowedFd<'_> {
                self.0.loop_().fd()
            }
        }
        event_loop
            .insert_source(
                Generic::new(LoopFd(main_loop), Interest::READ, Mode::Level),
                |_, wrapper, _| {
                    wrapper.0.loop_().iterate(Timeout::None);
                    Ok(PostAction::Continue)
                },
            )
            .ok()?;
        Some(Self {
            _context: context,
            core,
        })
    }

    /// Start a stream of `width` x `height` physical pixels for one
    /// session. The node id goes to the client once PipeWire assigns it.
    pub fn start_cast(
        &self,
        session_id: u64,
        target: CastTarget,
        width: i32,
        height: i32,
        hdr_available: bool,
        signal: crate::mutter::CastSignals,
    ) -> Option<Cast> {
        let session_signal = signal.clone();
        let stream = StreamRc::new(
            self.core.clone(),
            "tuna-screen-cast",
            PropertiesBox::from_iter([("node.name", "tuna-screen-cast")]),
        )
        .ok()?;
        let inner = Rc::new(RefCell::new(Inner {
            hdr_available,
            ..Inner::default()
        }));
        let listener = stream
            .add_local_listener_with_user_data(())
            .state_changed({
                let inner = inner.clone();
                move |stream, (), old, new| {
                    eprintln!("tuna-compositor: screen cast {session_id}: {old:?} -> {new:?}");
                    let mut inner = inner.borrow_mut();
                    match new {
                        StreamState::Paused => {
                            if inner.node_id.is_none() {
                                let id = stream.node_id();
                                inner.node_id = Some(id);
                                signal.stream_added(id);
                            }
                            inner.streaming = false;
                        }
                        StreamState::Streaming => inner.streaming = true,
                        StreamState::Error(_) => {
                            inner.streaming = false;
                            inner.failed = true;
                        }
                        StreamState::Unconnected | StreamState::Connecting => {}
                    }
                }
            })
            .param_changed({
                let inner = inner.clone();
                move |stream, (), id, pod| {
                    if ParamType::from_raw(id) != ParamType::Format {
                        return;
                    }
                    let Some(pod) = pod else { return };
                    let Ok((media_type, media_subtype)) = parse_format(pod) else {
                        return;
                    };
                    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
                        return;
                    }
                    let mut format = VideoInfoRaw::new();
                    if format.parse(pod).is_err() {
                        return;
                    }
                    let hdr_available = inner.borrow().hdr_available;
                    let Some(encoding) = CaptureFormat::from_negotiated(format, hdr_available)
                    else {
                        inner.borrow_mut().failed = true;
                        return;
                    };
                    let (w, h) = (format.size().width as i32, format.size().height as i32);
                    let stride = w * BYTES_PER_PIXEL;
                    // Shared memory the size of one frame, plus a header.
                    let buffers = pod::object!(
                        SpaTypes::ObjectParamBuffers,
                        ParamType::Buffers,
                        Property::new(
                            SPA_PARAM_BUFFERS_buffers,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Range {
                                    default: 4,
                                    min: 2,
                                    max: 8
                                }
                            ))),
                        ),
                        Property::new(SPA_PARAM_BUFFERS_blocks, pod::Value::Int(1)),
                        Property::new(SPA_PARAM_BUFFERS_size, pod::Value::Int(stride * h)),
                        Property::new(SPA_PARAM_BUFFERS_stride, pod::Value::Int(stride)),
                        Property::new(
                            SPA_PARAM_BUFFERS_dataType,
                            pod::Value::Choice(ChoiceValue::Int(Choice(
                                ChoiceFlags::empty(),
                                ChoiceEnum::Flags {
                                    default: 1 << SPA_DATA_MemFd,
                                    flags: vec![1 << SPA_DATA_MemFd],
                                },
                            ))),
                        ),
                    );
                    let header = pod::object!(
                        SpaTypes::ObjectParamMeta,
                        ParamType::Meta,
                        Property::new(
                            SPA_PARAM_META_type,
                            pod::Value::Id(pipewire::spa::utils::Id(SPA_META_Header))
                        ),
                        Property::new(
                            SPA_PARAM_META_size,
                            pod::Value::Int(std::mem::size_of::<spa_meta_header>() as i32)
                        ),
                    );
                    let (mut b1, mut b2) = (Vec::new(), Vec::new());
                    let mut params = [make_pod(&mut b1, buffers), make_pod(&mut b2, header)];
                    if stream.update_params(&mut params).is_ok() {
                        inner.borrow_mut().frame = Some(CaptureFrame {
                            size: (w, h),
                            format: encoding,
                        });
                    }
                }
            })
            .register()
            .ok()?;
        let mut buffers = [Vec::new(), Vec::new()];
        let (mut formats, count) =
            format_params(&mut buffers, width as u32, height as u32, hdr_available);
        stream
            .connect(
                Direction::Output,
                None,
                StreamFlags::DRIVER | StreamFlags::MAP_BUFFERS,
                &mut formats[..count],
            )
            .ok()?;
        Some(Cast {
            session_id,
            target,
            offered: (width, height),
            hdr_available,
            signal: session_signal,
            stream,
            _listener: listener,
            inner,
            last_frame: None,
            sequence: 0,
        })
    }
}

#[derive(Default)]
struct Inner {
    node_id: Option<u32>,
    streaming: bool,
    failed: bool,
    hdr_available: bool,
    /// Negotiated size and encoding, never inferred from the output.
    frame: Option<CaptureFrame>,
}

/// One running stream.
pub struct Cast {
    pub session_id: u64,
    /// Monitor or window this stream shows.
    pub target: CastTarget,
    /// Size last offered to the consumer (window casts follow resizes).
    offered: (i32, i32),
    hdr_available: bool,
    /// The stream's D-Bus signals, for ending the session.
    signal: crate::mutter::CastSignals,
    stream: StreamRc,
    _listener: StreamListener<()>,
    inner: Rc<RefCell<Inner>>,
    last_frame: Option<Instant>,
    sequence: u64,
}

impl Cast {
    /// Whether a frame is wanted now: streaming, negotiated, and at
    /// least [`FRAME_INTERVAL`] since the last one.
    pub fn wants_frame(&self) -> Option<CaptureFrame> {
        let inner = self.inner.borrow();
        if !inner.streaming {
            return None;
        }
        if self
            .last_frame
            .is_some_and(|t| t.elapsed() < FRAME_INTERVAL)
        {
            return None;
        }
        inner
            .frame
            .filter(|frame| frame.format != CaptureFormat::Hdr10 || inner.hdr_available)
    }

    /// Offer the consumer a new frame size (a cast window resized), the
    /// way niri does: a fresh format; frames keep the old size until the
    /// consumer accepts and new buffers arrive.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.offer(width, height, self.hdr_available);
    }

    pub fn set_hdr_available(&mut self, available: bool) {
        self.offer(self.offered.0, self.offered.1, available);
    }

    fn offer(&mut self, width: i32, height: i32, hdr_available: bool) {
        if (self.offered == (width, height) && self.hdr_available == hdr_available)
            || width <= 0
            || height <= 0
        {
            return;
        }
        self.inner.borrow_mut().hdr_available = hdr_available;
        let mut buffers = [Vec::new(), Vec::new()];
        let (mut formats, count) =
            format_params(&mut buffers, width as u32, height as u32, hdr_available);
        if self.stream.update_params(&mut formats[..count]).is_ok() {
            self.offered = (width, height);
            self.hdr_available = hdr_available;
        }
    }

    /// Tell the consumer its session ended (the cast window closed).
    pub fn close(&self) {
        self.signal.session_closed(self.session_id);
    }

    /// Whether PipeWire reported the stream broken.
    pub fn failed(&self) -> bool {
        self.inner.borrow().failed
    }

    /// Send one frame in the negotiated encoding (four bytes per pixel).
    pub fn send_frame(&mut self, width: i32, height: i32, pixels: &[u8]) {
        self.last_frame = Some(Instant::now());
        let stride = (width * BYTES_PER_PIXEL) as usize;
        let size = stride * height as usize;
        if pixels.len() < size {
            return;
        }
        unsafe {
            let buffer = self.stream.dequeue_raw_buffer();
            if buffer.is_null() {
                return;
            }
            let spa_buffer = (*buffer).buffer;
            let data = &mut *(*spa_buffer).datas;
            if !data.data.is_null() && data.maxsize as usize >= size {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), data.data as *mut u8, size);
                let chunk = &mut *data.chunk;
                chunk.offset = 0;
                chunk.size = size as u32;
                chunk.stride = stride as i32;
                chunk.flags = SPA_CHUNK_FLAG_NONE as i32;
            } else {
                (*data.chunk).size = 0;
                (*data.chunk).flags = SPA_CHUNK_FLAG_CORRUPTED as i32;
            }
            self.sequence = self.sequence.wrapping_add(1);
            let header = spa_buffer_find_meta_data(
                spa_buffer,
                SPA_META_Header,
                std::mem::size_of::<spa_meta_header>(),
            ) as *mut spa_meta_header;
            if !header.is_null() {
                (*header).flags = 0;
                (*header).seq = self.sequence;
                (*header).pts = -1;
            }
            pipewire::sys::pw_stream_queue_buffer(self.stream.as_raw_ptr(), buffer);
        }
    }
}

/// The `(x, y, width, height)` part of a 4-byte-per-pixel frame of
/// `size`, rows top-down; `None` when the area leaves the frame.
pub fn crop(
    pixels: &[u8],
    (frame_w, frame_h): (i32, i32),
    (x, y, w, h): (i32, i32, i32, i32),
) -> Option<Vec<u8>> {
    if x < 0 || y < 0 || w <= 0 || h <= 0 || x + w > frame_w || y + h > frame_h {
        return None;
    }
    let bpp = BYTES_PER_PIXEL as usize;
    let stride = frame_w as usize * bpp;
    if pixels.len() < stride * frame_h as usize {
        return None;
    }
    let row = w as usize * bpp;
    let mut out = Vec::with_capacity(row * h as usize);
    for line in y as usize..(y + h) as usize {
        let start = line * stride + x as usize * bpp;
        out.extend_from_slice(&pixels[start..start + row]);
    }
    Some(out)
}

fn video_format(width: u32, height: u32, encoding: CaptureFormat) -> pod::Object {
    let (format, primaries, transfer, flags) = match encoding {
        CaptureFormat::Sdr => (
            VideoFormat::BGRx,
            SPA_VIDEO_COLOR_PRIMARIES_BT709,
            SPA_VIDEO_TRANSFER_GAMMA22,
            pod::PropertyFlags::empty(),
        ),
        CaptureFormat::Hdr10 => (
            VideoFormat::xBGR_210LE,
            SPA_VIDEO_COLOR_PRIMARIES_BT2020,
            TRANSFER_SMPTE2084,
            pod::PropertyFlags::MANDATORY,
        ),
    };
    pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, format),
        // Mutter 51 monitor update_formats only adds HDR for an active
        // BT.2020/PQ output. push_format_object makes its colour state
        // mandatory; SDR consumers cannot silently select HDR bytes.
        Property::new(
            SPA_FORMAT_VIDEO_colorRange,
            pod::Value::Id(pipewire::spa::utils::Id(SPA_VIDEO_COLOR_RANGE_0_255)),
        ),
        Property::new(
            SPA_FORMAT_VIDEO_colorMatrix,
            pod::Value::Id(pipewire::spa::utils::Id(SPA_VIDEO_COLOR_MATRIX_RGB)),
        ),
        Property {
            key: SPA_FORMAT_VIDEO_colorPrimaries,
            value: pod::Value::Id(pipewire::spa::utils::Id(primaries)),
            flags,
        },
        Property {
            key: SPA_FORMAT_VIDEO_transferFunction,
            value: pod::Value::Id(pipewire::spa::utils::Id(transfer)),
            flags,
        },
        pod::property!(
            FormatProperties::VideoSize,
            Rectangle,
            Rectangle { width, height }
        ),
        pod::property!(
            FormatProperties::VideoFramerate,
            Fraction,
            Fraction { num: 0, denom: 1 }
        ),
        pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 1, denom: 1 },
            Fraction { num: 30, denom: 1 }
        ),
    )
}

fn format_params(
    buffers: &mut [Vec<u8>; 2],
    width: u32,
    height: u32,
    hdr_available: bool,
) -> ([&Pod; 2], usize) {
    let (sdr, hdr) = buffers.split_at_mut(1);
    let sdr = make_pod(&mut sdr[0], video_format(width, height, CaptureFormat::Sdr));
    if hdr_available {
        let hdr = make_pod(
            &mut hdr[0],
            video_format(width, height, CaptureFormat::Hdr10),
        );
        ([sdr, hdr], 2)
    } else {
        ([sdr, sdr], 1)
    }
}

fn make_pod(buffer: &mut Vec<u8>, object: pod::Object) -> &Pod {
    PodSerializer::serialize(
        std::io::Cursor::new(&mut *buffer),
        &pod::Value::Object(object),
    )
    .expect("pod serializes");
    Pod::from_bytes(buffer).expect("pod parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_receives_sdr_rgb_color_encoding_after_resize() {
        // Parse the real serialized EnumFormat POD with the same SPA
        // parser a PipeWire consumer uses, both initially and on resize.
        for (width, height) in [(1920, 1080), (640, 480)] {
            let mut buffer = Vec::new();
            let pod = make_pod(&mut buffer, video_format(width, height, CaptureFormat::Sdr));
            assert_eq!(
                parse_format(pod).unwrap(),
                (MediaType::Video, MediaSubtype::Raw)
            );
            let mut format = VideoInfoRaw::new();
            format.parse(pod).unwrap();
            assert_eq!(format.format(), VideoFormat::BGRx);
            assert_eq!(format.size(), Rectangle { width, height });
            assert_eq!(format.color_range(), SPA_VIDEO_COLOR_RANGE_0_255);
            assert_eq!(format.color_matrix(), SPA_VIDEO_COLOR_MATRIX_RGB);
            assert_eq!(format.color_primaries(), SPA_VIDEO_COLOR_PRIMARIES_BT709);
            assert_eq!(format.transfer_function(), SPA_VIDEO_TRANSFER_GAMMA22);
        }
    }

    #[test]
    fn hdr_offer_is_optional_precise_and_mandatory_to_the_consumer() {
        use pipewire::spa::pod::deserialize::PodDeserializer;
        for available in [false, true] {
            let mut buffers = [Vec::new(), Vec::new()];
            let (formats, count) = format_params(&mut buffers, 1920, 1080, available);
            assert_eq!(count, if available { 2 } else { 1 });
            let mut sdr = VideoInfoRaw::new();
            sdr.parse(formats[0]).unwrap();
            assert_eq!(
                CaptureFormat::from_negotiated(sdr, available),
                Some(CaptureFormat::Sdr)
            );
            if !available {
                continue;
            }
            let mut hdr = VideoInfoRaw::new();
            hdr.parse(formats[1]).unwrap();
            assert_eq!(hdr.format(), VideoFormat::xBGR_210LE);
            assert_eq!(hdr.color_range(), SPA_VIDEO_COLOR_RANGE_0_255);
            assert_eq!(hdr.color_matrix(), SPA_VIDEO_COLOR_MATRIX_RGB);
            assert_eq!(hdr.color_primaries(), SPA_VIDEO_COLOR_PRIMARIES_BT2020);
            assert_eq!(hdr.transfer_function(), TRANSFER_SMPTE2084);
            assert_eq!(
                CaptureFormat::from_negotiated(hdr, true),
                Some(CaptureFormat::Hdr10)
            );
            assert_eq!(CaptureFormat::from_negotiated(hdr, false), None);
            let (_, value) = PodDeserializer::deserialize_any_from(formats[1].as_bytes()).unwrap();
            let pod::Value::Object(object) = value else {
                panic!("expected format object")
            };
            for key in [
                SPA_FORMAT_VIDEO_colorPrimaries,
                SPA_FORMAT_VIDEO_transferFunction,
            ] {
                let property = object
                    .properties
                    .iter()
                    .find(|property| property.key == key)
                    .unwrap();
                assert!(property.flags.contains(pod::PropertyFlags::MANDATORY));
            }
            hdr.set_transfer_function(SPA_VIDEO_TRANSFER_GAMMA22);
            assert_eq!(
                CaptureFormat::from_negotiated(hdr, true),
                None,
                "never label gamma22 bytes PQ"
            );
            sdr.set_transfer_function(TRANSFER_SMPTE2084);
            assert_eq!(
                CaptureFormat::from_negotiated(sdr, true),
                None,
                "never label 8-bit SDR as HDR"
            );
        }
    }

    #[cfg(feature = "drm")]
    #[test]
    fn hdr_capture_readback_keeps_highlights_and_sdr_reference_white() {
        use crate::color_state::{self, ColorState};
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        use smithay::backend::renderer::gles::GlesRenderer;
        use smithay::backend::renderer::{Bind, ExportMem, Frame, ImportMem, Renderer};
        let renderer = (|| {
            let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
            let context = EGLContext::new(&display).ok()?;
            unsafe { GlesRenderer::new(context) }.ok()
        })();
        let Some(mut renderer) = renderer else {
            assert!(
                std::env::var_os("TUNA_REQUIRE_COLOR_GL").is_none(),
                "HDR capture requires EGL"
            );
            eprintln!("HDR capture GL test skipped: no EGL");
            return;
        };
        let mut capture = match HdrCapture::new(&mut renderer, (3, 1).into()) {
            Ok(capture) => capture,
            Err(error) => {
                assert!(
                    std::env::var_os("TUNA_REQUIRE_COLOR_GL").is_none(),
                    "{error}"
                );
                eprintln!("HDR capture GL test skipped: {error}");
                return;
            }
        };
        let pq = ColorState::params(color_state::hdr_output_params());
        let decode = color_state::transform(&pq, &capture.scene.blending).unwrap();
        let code = 192.0 / 255.0;
        let highlight = decode.apply([code, code, code, 1.0]);
        assert!(highlight[0] > 1.0);
        let samples = [highlight, [1.0, 1.0, 1.0, 1.0], [0.1, 0.1, 0.1, 1.0]];
        let source = renderer
            .import_memory(
                &[192, 192, 192, 255],
                smithay::backend::allocator::Fourcc::Abgr8888,
                (1, 1).into(),
                false,
            )
            .unwrap();
        let program = crate::color_render::final_output_program(&mut renderer).unwrap();
        let uniforms = crate::color_render::final_output_uniforms(&decode);
        {
            let mut target = renderer.bind(&mut capture.scene.texture).unwrap();
            let mut frame = renderer
                .render(
                    &mut target,
                    (3, 1).into(),
                    smithay::utils::Transform::Normal,
                )
                .unwrap();
            for (x, color) in samples.iter().enumerate().skip(1) {
                frame
                    .clear(
                        (*color).into(),
                        &[smithay::utils::Rectangle::new(
                            (x as i32, 0).into(),
                            (1, 1).into(),
                        )],
                    )
                    .unwrap();
            }
            let destination = smithay::utils::Rectangle::new((0, 0).into(), (1, 1).into());
            frame
                .render_texture_from_to(
                    &source,
                    smithay::utils::Rectangle::<f64, smithay::utils::Buffer>::from_size(
                        (1.0, 1.0).into(),
                    ),
                    destination,
                    &[destination],
                    &[],
                    smithay::utils::Transform::Normal,
                    1.0,
                    Some(&program),
                    &uniforms,
                )
                .unwrap();
            frame.finish().unwrap().wait().unwrap();
        }
        let mut target = renderer.bind(&mut capture.output).unwrap();
        {
            let mut frame = renderer
                .render(
                    &mut target,
                    (3, 1).into(),
                    smithay::utils::Transform::Normal,
                )
                .unwrap();
            capture.scene.present(&mut frame).unwrap();
            frame.finish().unwrap().wait().unwrap();
        }
        let mapping = renderer
            .copy_framebuffer(
                &target,
                smithay::utils::Rectangle::from_size((3, 1).into()),
                CaptureFormat::Hdr10.fourcc(),
            )
            .unwrap();
        let pixels = renderer.map_texture(&mapping).unwrap();
        let encode = color_state::transform(&capture.scene.blending, &pq).unwrap();
        let mut red = Vec::new();
        for (sample, bytes) in samples.iter().zip(pixels.as_chunks::<4>().0) {
            let word = u32::from_le_bytes(*bytes);
            let actual = [word & 1023, (word >> 10) & 1023, (word >> 20) & 1023];
            let expected = encode.apply(*sample);
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!((actual as f32 - expected.clamp(0.0, 1.0) * 1023.0).abs() <= 3.0);
            }
            red.push(word & 1023);
        }
        assert_eq!(red.len(), 3);
        assert!(
            red[0] > red[1] + 100,
            "highlights must not clip to SDR white"
        );
    }

    #[test]
    fn areas_crop_out_of_frames() {
        // A 3x2 frame whose pixels count up.
        let frame: Vec<u8> = (0u8..6).flat_map(|p| [p, p, p, 255]).collect();
        let cropped = crop(&frame, (3, 2), (1, 0, 2, 2)).unwrap();
        let firsts: Vec<u8> = cropped.chunks(4).map(|p| p[0]).collect();
        assert_eq!(firsts, [1, 2, 4, 5]);
        assert!(
            crop(&frame, (3, 2), (2, 0, 2, 1)).is_none(),
            "past the edge"
        );
        assert!(crop(&frame, (3, 2), (0, 0, 0, 1)).is_none(), "empty");
    }
}
