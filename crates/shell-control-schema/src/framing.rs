//! Message-agnostic frame codec (ADR 0002 envelope, ADR 0004 seam 1).
//!
//! A frame is a `u32` little-endian body length followed by a postcard
//! body of at most [`MAX_FRAME_BYTES`]. This module knows nothing about the
//! Tuna message schema: it encodes and decodes any serde type, and its
//! errors describe the envelope only. Message validation, title and
//! collection bounds, and version negotiation live in the crate root.
//!
//! Keep it that way: this file must not import anything from the rest of
//! the crate (`tests/framing_tests.rs` checks).

use serde::{Deserialize, Serialize};

/// Length of the `u32` little-endian body-length prefix.
pub const PREFIX_LEN: usize = 4;

/// Maximum accepted frame body size in bytes (1 MiB, per ADR 0002).
///
/// Enforced in [`decode`] and [`complete_frame_len`] before postcard
/// decoding, so a hostile or buggy peer cannot force a large allocation.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Envelope failure: the bytes are not one well-formed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer than [`PREFIX_LEN`] bytes, or a body shorter than the prefix
    /// claims.
    Truncated {
        /// Bytes required.
        expected: usize,
        /// Bytes present.
        actual: usize,
    },
    /// Declared body length exceeds [`MAX_FRAME_BYTES`]. Checked before any
    /// postcard decoding, so the declared length alone triggers it.
    Oversize {
        /// Declared body length.
        len: usize,
        /// The cap ([`MAX_FRAME_BYTES`]).
        max: usize,
    },
    /// postcard decode failure or trailing bytes after the declared body.
    Malformed(postcard::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { expected, actual } => {
                write!(f, "truncated frame: need {expected} bytes, have {actual}")
            }
            Self::Oversize { len, max } => {
                write!(f, "frame of {len} bytes exceeds cap of {max} bytes")
            }
            Self::Malformed(e) => write!(f, "malformed frame body: {e}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(e) => Some(e),
            _ => None,
        }
    }
}

impl From<postcard::Error> for FrameError {
    fn from(e: postcard::Error) -> Self {
        Self::Malformed(e)
    }
}

/// Read the declared body length from the front of `buf`.
///
/// `Ok(None)` while fewer than [`PREFIX_LEN`] bytes are present;
/// [`FrameError::Oversize`] as soon as the prefix declares more than
/// [`MAX_FRAME_BYTES`].
pub fn body_len(buf: &[u8]) -> Result<Option<usize>, FrameError> {
    let Some(prefix) = buf.first_chunk::<PREFIX_LEN>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::Oversize {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    Ok(Some(len))
}

/// Admission check for a streaming read buffer.
///
/// `Ok(Some(n))` when the first `n` bytes of `buf` hold one complete frame
/// (prefix and body), `Ok(None)` when more bytes are needed, and
/// [`FrameError::Oversize`] when the prefix declares more than
/// [`MAX_FRAME_BYTES`]. Pass `&buf[..n]` to [`decode`].
pub fn complete_frame_len(buf: &[u8]) -> Result<Option<usize>, FrameError> {
    Ok(body_len(buf)?
        .map(|len| PREFIX_LEN + len)
        .filter(|&total| buf.len() >= total))
}

/// Encode `value` as one frame: `u32` little-endian body length followed
/// by the postcard body.
///
/// Panics only if the value cannot be encoded or exceeds
/// [`MAX_FRAME_BYTES`]; both indicate a local programming error.
pub fn encode<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    // Grow-on-demand so small control messages avoid the 1 MiB buffer.
    // postcard::to_slice reports SerializeBufferFull when the buffer is
    // too small; that is the only recoverable encode error here.
    let mut cap = 1024usize;
    loop {
        let mut buf = vec![0u8; cap];
        match postcard::to_slice(value, &mut buf) {
            Ok(used) => {
                let body_len = used.len();
                let mut frame = Vec::with_capacity(PREFIX_LEN + body_len);
                frame.extend_from_slice(&(body_len as u32).to_le_bytes());
                frame.extend_from_slice(used);
                return frame;
            }
            Err(postcard::Error::SerializeBufferFull) => {
                cap *= 4;
                assert!(
                    cap <= MAX_FRAME_BYTES,
                    "encoded message exceeds {MAX_FRAME_BYTES}-byte frame cap"
                );
            }
            Err(e) => panic!("shell control encode failed: {e}"),
        }
    }
}

/// Decode exactly one frame into a `T`.
///
/// Checks run in wire order: prefix present, declared length within
/// [`MAX_FRAME_BYTES`] (before any postcard work), body complete, and body
/// well-formed with no trailing bytes.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, FrameError> {
    let Some(len) = body_len(bytes)? else {
        return Err(FrameError::Truncated {
            expected: PREFIX_LEN,
            actual: bytes.len(),
        });
    };
    let body = &bytes[PREFIX_LEN..];
    if body.len() < len {
        return Err(FrameError::Truncated {
            expected: len,
            actual: body.len(),
        });
    }
    if body.len() > len {
        return Err(FrameError::Malformed(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    let (value, trailing): (T, &[u8]) = postcard::take_from_bytes(body)?;
    if !trailing.is_empty() {
        return Err(FrameError::Malformed(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    Ok(value)
}
