// Tests for the message-agnostic frame codec (ADR 0004 seam 1).

use serde::{Deserialize, Serialize};
use tuna_shell_control::framing::{
    body_len, complete_frame_len, decode, encode, FrameError, MAX_FRAME_BYTES, PREFIX_LEN,
};
use tuna_shell_control::{decode_frame, encode_frame, DecodeError, Message};

/// A payload that is not part of the Tuna schema.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Foreign {
    id: u32,
    name: String,
}

fn foreign() -> Foreign {
    Foreign {
        id: 7,
        name: "not a tuna message".into(),
    }
}

#[test]
fn framing_source_imports_nothing_from_the_crate() {
    let src = include_str!("../src/framing.rs");
    for forbidden in ["crate::", "super::", "Message", "CommandKind"] {
        assert!(
            !src.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains(forbidden)),
            "framing.rs must stay message-agnostic; found `{forbidden}`"
        );
    }
}

#[test]
fn roundtrips_a_type_outside_the_schema() {
    let frame = encode(&foreign());
    assert_eq!(
        u32::from_le_bytes(frame[..PREFIX_LEN].try_into().unwrap()) as usize,
        frame.len() - PREFIX_LEN
    );
    assert_eq!(decode::<Foreign>(&frame).unwrap(), foreign());
}

#[test]
fn short_prefix_is_truncated() {
    assert_eq!(
        decode::<Foreign>(&[1, 0]),
        Err(FrameError::Truncated {
            expected: PREFIX_LEN,
            actual: 2
        })
    );
}

#[test]
fn oversize_prefix_rejected_before_body() {
    let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
    let oversize = FrameError::Oversize {
        len: MAX_FRAME_BYTES + 1,
        max: MAX_FRAME_BYTES,
    };
    assert_eq!(decode::<Foreign>(&prefix), Err(oversize.clone()));
    assert_eq!(body_len(&prefix), Err(oversize.clone()));
    assert_eq!(complete_frame_len(&prefix), Err(oversize));
}

#[test]
fn short_body_is_truncated() {
    let frame = encode(&foreign());
    let cut = &frame[..frame.len() - 1];
    assert_eq!(
        decode::<Foreign>(cut),
        Err(FrameError::Truncated {
            expected: frame.len() - PREFIX_LEN,
            actual: frame.len() - PREFIX_LEN - 1,
        })
    );
}

#[test]
fn bytes_after_the_declared_body_are_malformed() {
    let mut frame = encode(&foreign());
    frame.push(0);
    assert!(matches!(
        decode::<Foreign>(&frame),
        Err(FrameError::Malformed(_))
    ));
}

#[test]
fn complete_frame_len_waits_for_the_whole_frame() {
    let frame = encode(&foreign());
    assert_eq!(complete_frame_len(&[]), Ok(None));
    assert_eq!(complete_frame_len(&frame[..3]), Ok(None));
    assert_eq!(complete_frame_len(&frame[..frame.len() - 1]), Ok(None));
    assert_eq!(complete_frame_len(&frame), Ok(Some(frame.len())));

    let mut two = frame.clone();
    two.extend_from_slice(&frame);
    assert_eq!(complete_frame_len(&two), Ok(Some(frame.len())));
}

#[test]
fn decode_frame_maps_envelope_errors_unchanged() {
    let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
    assert_eq!(
        decode_frame(&prefix),
        Err(DecodeError::Oversize {
            len: MAX_FRAME_BYTES + 1,
            max: MAX_FRAME_BYTES,
        })
    );
    assert_eq!(
        decode_frame(&[0]),
        Err(DecodeError::Truncated {
            expected: PREFIX_LEN,
            actual: 1
        })
    );
    let err = FrameError::Malformed(postcard::Error::DeserializeBadEncoding);
    assert_eq!(
        DecodeError::from(err.clone()),
        DecodeError::Malformed(postcard::Error::DeserializeBadEncoding)
    );
    assert_eq!(DecodeError::from(err.clone()).to_string(), err.to_string());
}

#[test]
fn encode_frame_is_the_generic_codec_applied_to_message() {
    let msg = Message::Overview { open: true };
    assert_eq!(encode_frame(&msg), encode(&msg));
    assert_eq!(decode::<Message>(&encode_frame(&msg)).unwrap(), msg);
}
