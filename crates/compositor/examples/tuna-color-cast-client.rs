//! Actual ten-bit HDR PipeWire consumer for the colour capture qualification.
//!
//! Usage: tuna-color-cast-client NODE_ID RAW_PATH
//! The authorized supervised shell creates the capture session and delegates
//! its node to this consumer. This program acquires no D-Bus role or grant.
//! It negotiates xBGR_210LE / full-range RGB / BT.2020 / SMPTE ST 2084,
//! validates the actual Format event, saves one mapped frame and prints JSON.
//! There is deliberately no GStreamer colour conversion: its PipeWire plugin
//! does not map this packed format on all supported distributions.
//! The supervised Python fixture also uses --shell-hello,
//! --keep-display-change ID and --inspect-shell-frame as canonical wire
//! codecs. These modes never connect a socket or acquire shell authority.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use pipewire as pw;
use pw::spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use pw::spa::param::format_utils::parse_format;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::param::ParamType;
use pw::spa::pod::{self, Pod, Property};
use pw::spa::sys::*;
use pw::spa::utils::{Direction, Id, SpaTypes};
use serde_json::{json, Value};

// Stable SPA ABI enumerator from upstream spa/param/video/color.h;
// older distro build headers stop before SMPTE2084 was named.
const TRANSFER_SMPTE2084: u32 = 14;
type Outcome = Rc<RefCell<Option<Result<Value, String>>>>;

fn hdr_format() -> pod::Object {
    pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pod::property!(FormatProperties::VideoFormat, Id, VideoFormat::xBGR_210LE),
        Property::new(
            SPA_FORMAT_VIDEO_colorRange,
            pod::Value::Id(Id(SPA_VIDEO_COLOR_RANGE_0_255))
        ),
        Property::new(
            SPA_FORMAT_VIDEO_colorMatrix,
            pod::Value::Id(Id(SPA_VIDEO_COLOR_MATRIX_RGB))
        ),
        Property {
            key: SPA_FORMAT_VIDEO_colorPrimaries,
            flags: pod::PropertyFlags::MANDATORY,
            value: pod::Value::Id(Id(SPA_VIDEO_COLOR_PRIMARIES_BT2020)),
        },
        Property {
            key: SPA_FORMAT_VIDEO_transferFunction,
            flags: pod::PropertyFlags::MANDATORY,
            value: pod::Value::Id(Id(TRANSFER_SMPTE2084)),
        },
    )
}

fn parse_hdr(pod: &Pod) -> Result<VideoInfoRaw, String> {
    if parse_format(pod).map_err(|error| format!("media format: {error:?}"))?
        != (MediaType::Video, MediaSubtype::Raw)
    {
        return Err("negotiated non-raw-video format".into());
    }
    let mut info = VideoInfoRaw::new();
    info.parse(pod)
        .map_err(|error| format!("video format: {error:?}"))?;
    if info.format() != VideoFormat::xBGR_210LE
        || info.color_range() != SPA_VIDEO_COLOR_RANGE_0_255
        || info.color_matrix() != SPA_VIDEO_COLOR_MATRIX_RGB
        || info.color_primaries() != SPA_VIDEO_COLOR_PRIMARIES_BT2020
        || info.transfer_function() != TRANSFER_SMPTE2084
        || info.size().width == 0
        || info.size().height == 0
    {
        return Err(format!(
            "consumer did not negotiate ten-bit BT.2020/PQ RGB: {info:?}"
        ));
    }
    Ok(info)
}

fn consume(
    data: &mut pw::spa::buffer::Data,
    info: VideoInfoRaw,
    raw_path: &Path,
) -> Result<Value, String> {
    let width = info.size().width as usize;
    let height = info.size().height as usize;
    let stride = usize::try_from(data.chunk().stride()).map_err(|_| "negative frame stride")?;
    let offset = data.chunk().offset() as usize;
    let size = data.chunk().size() as usize;
    if stride < width * 4 || size < stride * (height - 1) + width * 4 {
        return Err("short mapped frame or invalid stride".into());
    }
    let bytes = data.data().ok_or("PipeWire did not map the frame")?;
    let frame = bytes
        .get(offset..offset.checked_add(size).ok_or("frame length overflow")?)
        .ok_or("chunk leaves mapped PipeWire buffer")?;
    let centre = (height / 2) * stride + (width / 2) * 4;
    let pixel: [u8; 4] = frame
        .get(centre..centre + 4)
        .ok_or("missing centre pixel")?
        .try_into()
        .map_err(|_| "invalid packed pixel")?;
    let word = u32::from_le_bytes(pixel);
    let rgb = [word & 1023, (word >> 10) & 1023, (word >> 20) & 1023];
    std::fs::write(raw_path, frame).map_err(|error| error.to_string())?;
    Ok(json!({
        "format": "xBGR_210LE", "format_id": info.format().as_raw(),
        "color_range": info.color_range(), "color_matrix": info.color_matrix(),
        "color_primaries": info.color_primaries(), "transfer_function": info.transfer_function(),
        "width": width, "height": height, "stride": stride,
        "source_chunk_offset": offset, "raw_bytes": size,
        "centre_rgb10": rgb, "raw_path": raw_path,
    }))
}

#[derive(Default)]
struct Negotiated {
    info: Option<VideoInfoRaw>,
}

fn capture(node: u32, raw_path: PathBuf) -> Result<Value, String> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(|error| error.to_string())?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).map_err(|error| error.to_string())?;
    let core = context
        .connect_rc(None)
        .map_err(|error| error.to_string())?;
    let stream = pw::stream::StreamRc::new(
        core,
        "tuna-colour-hdr-consumer",
        pw::properties::PropertiesBox::from_iter([
            ("media.type", "Video"),
            ("media.category", "Capture"),
            ("media.role", "Screen"),
        ]),
    )
    .map_err(|error| error.to_string())?;
    let outcome: Outcome = Rc::new(RefCell::new(None));
    let _listener = stream
        .add_local_listener_with_user_data(Negotiated::default())
        .state_changed({
            let outcome = outcome.clone();
            let main_loop = main_loop.clone();
            move |_, _, _, state| {
                if let pw::stream::StreamState::Error(error) = state {
                    *outcome.borrow_mut() = Some(Err(format!("PipeWire stream: {error}")));
                    main_loop.quit();
                }
            }
        })
        .param_changed({
            let outcome = outcome.clone();
            let main_loop = main_loop.clone();
            move |_, negotiated, id, pod| {
                if ParamType::from_raw(id) != ParamType::Format {
                    return;
                }
                let Some(pod) = pod else {
                    negotiated.info = None;
                    return;
                };
                match parse_hdr(pod) {
                    Ok(info) => negotiated.info = Some(info),
                    Err(error) => {
                        *outcome.borrow_mut() = Some(Err(error));
                        main_loop.quit();
                    }
                }
            }
        })
        .process({
            let outcome = outcome.clone();
            let main_loop = main_loop.clone();
            move |stream, negotiated| {
                if outcome.borrow().is_some() {
                    return;
                }
                let Some(info) = negotiated.info else { return };
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                if data.chunk().size() == 0 {
                    return;
                }
                *outcome.borrow_mut() = Some(consume(data, info, &raw_path));
                main_loop.quit();
            }
        })
        .register()
        .map_err(|error| error.to_string())?;
    let timer = main_loop.loop_().add_timer({
        let outcome = outcome.clone();
        let main_loop = main_loop.clone();
        move |_| {
            if outcome.borrow().is_none() {
                *outcome.borrow_mut() =
                    Some(Err("timed out waiting for an actual HDR frame".into()));
            }
            main_loop.quit();
        }
    });
    timer
        .update_timer(Some(Duration::from_secs(15)), None)
        .into_result()
        .map_err(|error| format!("consumer timer: {error:?}"))?;
    let bytes = pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pod::Value::Object(hdr_format()),
    )
    .map_err(|error| format!("consumer format: {error:?}"))?
    .0
    .into_inner();
    let pod = Pod::from_bytes(&bytes).ok_or("consumer format POD is invalid")?;
    stream
        .connect(
            Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(|error| error.to_string())?;
    main_loop.run();
    let result = outcome
        .borrow_mut()
        .take()
        .ok_or("consumer exited without a frame")?;
    result
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().expect("NODE_ID RAW_PATH or shell codec mode");
    if first == "--shell-hello" {
        assert!(args.next().is_none());
        std::io::stdout()
            .write_all(&tuna_shell_control::encode_frame(
                &tuna_shell_control::Message::Hello {
                    version: tuna_shell_control::CURRENT_VERSION,
                },
            ))
            .unwrap();
        return;
    }
    if first == "--keep-display-change" {
        let id = args
            .next()
            .expect("command ID")
            .to_str()
            .expect("numeric ID")
            .parse()
            .expect("numeric ID");
        assert!(args.next().is_none());
        std::io::stdout()
            .write_all(&tuna_shell_control::encode_frame(
                &tuna_shell_control::Message::Command {
                    id,
                    kind: tuna_shell_control::CommandKind::CompleteDisplayChange { keep: true },
                },
            ))
            .unwrap();
        return;
    }
    if first == "--inspect-shell-frame" {
        use tuna_shell_control::{CommandStatus, Message};
        assert!(args.next().is_none());
        let mut frame = Vec::new();
        std::io::stdin()
            .take((tuna_shell_control::MAX_FRAME_BYTES + 5) as u64)
            .read_to_end(&mut frame)
            .unwrap();
        let message =
            tuna_shell_control::decode_frame(&frame).expect("valid canonical control frame");
        let evidence = match message {
            Message::Hello { version } => {
                json!({"kind": "hello", "major": version.major, "minor": version.minor})
            }
            Message::Snapshot { locked, .. } => json!({"kind": "snapshot", "locked": locked}),
            Message::ConfirmDisplayChange { timeout_secs } => {
                json!({"kind": "confirm-display", "timeout_secs": timeout_secs})
            }
            Message::CommandResult { id, status } => {
                json!({"kind": "command-result", "id": id, "applied": status == CommandStatus::Applied})
            }
            _ => json!({"kind": "other"}),
        };
        println!("{evidence}");
        return;
    }
    let node: u32 = first
        .to_str()
        .expect("numeric NODE_ID")
        .parse()
        .expect("numeric NODE_ID");
    let path = PathBuf::from(args.next().expect("NODE_ID RAW_PATH"));
    assert!(args.next().is_none(), "NODE_ID RAW_PATH");
    match capture(node, path) {
        Ok(evidence) => println!("{evidence}"),
        Err(error) => {
            eprintln!("HDR PipeWire consumer failed: {error}");
            std::process::exit(1);
        }
    }
}
