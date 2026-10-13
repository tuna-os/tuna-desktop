//! Bounded interactive lock authentication, appended in protocol 0.36.
//! Keys identify an attempt; only the server can grant a lock transition.
use serde::de::{Error, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

pub const MAX_MESSAGES: usize = 32;
pub const MAX_TEXT_BYTES: usize = 4096;

/// Policy support and start readiness are distinct: a busy common Slot does
/// not revoke an already admitted conversation's exact key/lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability {
    pub epoch: u64,
    pub policy_available: bool,
    pub slot_idle: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    SessionStack,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub start_request: u64,
    pub attempt_serial: u64,
    pub epoch: u64,
}
impl Key {
    pub fn valid(self) -> bool {
        self.start_request != 0 && self.attempt_serial != 0 && self.epoch != u64::MAX
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Style {
    Visible,
    Secret,
    Info,
    Error,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Starting,
    Prompt,
    Verifying,
    Cancelling,
    Denied,
    Unavailable,
    Blocked,
}

fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Text;
    impl Visitor<'_> for Text {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded authentication text")
        }
        fn visit_str<E: Error>(self, value: &str) -> Result<String, E> {
            if value.len() > MAX_TEXT_BYTES || value.contains('\0') {
                return Err(E::custom("authentication text bound"));
            }
            Ok(value.to_owned())
        }
    }
    d.deserialize_str(Text)
}
/// Owned response bytes; no Debug disclosure and wipe on every drop/error.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct AnswerText(pub String);
impl<'de> Deserialize<'de> for AnswerText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        text(d).map(Self)
    }
}
impl fmt::Debug for AnswerText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AnswerText(<redacted>)")
    }
}
impl Drop for AnswerText {
    fn drop(&mut self) {
        self.wipe();
    }
}
impl AnswerText {
    fn wipe(&mut self) {
        // SAFETY: exclusively owned bytes are replaced with valid UTF-8 NULs.
        crate::wipe_auth_bytes(unsafe { self.0.as_mut_vec() });
    }
    pub fn into_secret(mut self) -> crate::Secret {
        crate::Secret(std::mem::take(&mut self.0))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub index: u32,
    pub text: Option<AnswerText>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub index: u32,
    pub style: Style,
    #[serde(deserialize_with = "text")]
    pub text: String,
}

fn bounded<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct List<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for List<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("at most 32 indexed authentication values")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Vec<T>, A::Error> {
            if a.size_hint().is_some_and(|size| size > MAX_MESSAGES) {
                return Err(A::Error::custom("authentication count bound"));
            }
            let mut values = Vec::with_capacity(a.size_hint().unwrap_or(0).min(MAX_MESSAGES));
            while let Some(value) = a.next_element()? {
                if values.len() == MAX_MESSAGES {
                    return Err(A::Error::custom("authentication count bound"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(List(std::marker::PhantomData))
}
pub fn answers<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Answer>, D::Error> {
    bounded(d)
}
pub fn prompts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Prompt>, D::Error> {
    bounded(d)
}
pub fn valid_answers(values: &[Answer]) -> bool {
    !values.is_empty()
        && values.len() <= MAX_MESSAGES
        && values.iter().enumerate().all(|(i, a)| {
            a.index as usize == i
                && a.text
                    .as_ref()
                    .is_none_or(|t| t.0.len() <= MAX_TEXT_BYTES && !t.0.contains('\0'))
        })
}
pub fn valid_prompts(values: &[Prompt]) -> bool {
    !values.is_empty()
        && values.len() <= MAX_MESSAGES
        && values.iter().enumerate().all(|(i, a)| {
            a.index as usize == i && a.text.len() <= MAX_TEXT_BYTES && !a.text.contains('\0')
        })
}

/// Single wiped credential frame for Stage C's no-replay writer. Input values
/// are owned here and wiped even if bounds/encoding refuse the operation.
pub fn encode_answer_frame(
    id: u64,
    key: Key,
    sequence: u32,
    answers: Vec<Answer>,
) -> Result<crate::SensitiveFrame, crate::ControlError> {
    let message = crate::Message::Command {
        id,
        kind: crate::CommandKind::AuthAnswer {
            key,
            sequence,
            answers,
        },
    };
    crate::validate_titles(&message).map_err(crate::ControlError::Decode)?;
    let crate::Message::Command {
        kind: crate::CommandKind::AuthAnswer { answers, .. },
        ..
    } = &message
    else {
        unreachable!()
    };
    let cap = answers
        .iter()
        .map(|a| a.text.as_ref().map_or(0, |t| t.0.len()) + 32)
        .sum::<usize>()
        + 64;
    let mut frame = crate::SensitiveFrame(vec![0; cap + 4]);
    let used = postcard::to_slice(&message, &mut frame.0[4..])
        .map_err(|_| crate::ControlError::Unexpected("authentication encoding refused".into()))?
        .len();
    frame.0[..4].copy_from_slice(&(used as u32).to_le_bytes());
    frame.0.truncate(used + 4);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_frame, encode_frame, CommandKind, Message};

    fn key() -> Key {
        Key {
            start_request: 1,
            attempt_serial: 2,
            epoch: 3,
        }
    }
    fn values() -> Vec<Answer> {
        vec![
            Answer {
                index: 0,
                text: Some(AnswerText("owned-response".into())),
            },
            Answer {
                index: 1,
                text: None,
            },
        ]
    }
    #[test]
    fn dedicated_answer_encoder_matches_postcard_and_wipes_owned_buffers() {
        let expected = encode_frame(&Message::Command {
            id: 4,
            kind: CommandKind::AuthAnswer {
                key: key(),
                sequence: 1,
                answers: values(),
            },
        });
        let mut actual = encode_answer_frame(4, key(), 1, values()).unwrap();
        assert_eq!(actual.as_bytes(), expected);
        assert!(matches!(
            decode_frame(actual.as_bytes()).unwrap(),
            Message::Command {
                kind: CommandKind::AuthAnswer { sequence: 1, .. },
                ..
            }
        ));
        assert!(!format!("{actual:?}").contains("owned-response"));
        actual.wipe();
        assert!(actual.as_bytes().iter().all(|byte| *byte == 0));
        let mut text = AnswerText("owned-response".into());
        assert!(!format!("{text:?}").contains("owned-response"));
        text.wipe();
        assert!(text.0.as_bytes().iter().all(|byte| *byte == 0));
    }
    #[test]
    fn authentication_bounds_and_indices_are_checked_by_real_decoder() {
        let check = |answers| {
            decode_frame(&encode_frame(&Message::Command {
                id: 4,
                kind: CommandKind::AuthAnswer {
                    key: key(),
                    sequence: 1,
                    answers,
                },
            }))
        };
        assert!(check(values()).is_ok());
        assert!(check(vec![]).is_err());
        assert!(check(vec![Answer {
            index: 1,
            text: None
        }])
        .is_err());
        assert!(check((0..33).map(|index| Answer { index, text: None }).collect()).is_err());
        for text in ["x".repeat(MAX_TEXT_BYTES + 1), "bad\0answer".into()] {
            assert!(check(vec![Answer {
                index: 0,
                text: Some(AnswerText(text))
            }])
            .is_err());
        }
        for sequence in [0, 65] {
            assert!(encode_answer_frame(4, key(), sequence, values()).is_err());
        }
        assert!(encode_answer_frame(
            4,
            Key {
                attempt_serial: 0,
                ..key()
            },
            1,
            values()
        )
        .is_err());
        let mut extra = encode_answer_frame(4, key(), 1, values())
            .unwrap()
            .as_bytes()
            .to_vec();
        extra.push(0);
        assert!(decode_frame(&extra).is_err());
    }
    #[test]
    fn oversized_wire_collection_is_refused_before_deserializing_any_member() {
        let calls = std::cell::Cell::new(0);
        let input = (0u32..33).inspect(|_| calls.set(calls.get() + 1));
        let reader = serde::de::value::SeqDeserializer::<_, serde::de::value::Error>::new(input);
        assert!(bounded::<_, u32>(reader).is_err());
        assert_eq!(calls.get(), 0);
    }
    #[test]
    fn capability_is_appended_and_invalid_epoch_or_bool_are_refused() {
        let message = Message::AuthAvailability(Availability {
            epoch: 7,
            policy_available: true,
            slot_idle: false,
        });
        let golden = vec![4, 0, 0, 0, 20, 7, 1, 0];
        assert_eq!(encode_frame(&message), golden);
        assert_eq!(decode_frame(&golden).unwrap(), message);
        assert!(
            decode_frame(&encode_frame(&Message::AuthAvailability(Availability {
                epoch: u64::MAX,
                policy_available: false,
                slot_idle: false
            })))
            .is_err()
        );
        assert!(decode_frame(&[4, 0, 0, 0, 20, 7, 2, 0]).is_err());
    }
    #[test]
    fn unknown_methods_styles_and_root_aliases_are_refused() {
        for body in [vec![3, 1, 20, 99], vec![19, 1, 2, 3, 1, 1, 0, 99, 0]] {
            let mut frame = (body.len() as u32).to_le_bytes().to_vec();
            frame.extend(body);
            assert!(decode_frame(&frame).is_err());
        }
        assert!(encode_answer_frame(1, key(), 1, values()).is_err());
        assert!(encode_answer_frame(0, key(), 1, values()).is_err());
    }
    #[test]
    fn legacy_message_and_snapshot_bytes_remain_035_compatible() {
        // Literal old postcard frames include the prior enum tails, followed
        // by the first appended variants. Snapshot has no new auth fields.
        for (message, body) in [
            (
                Message::Hello {
                    version: crate::ProtocolVersion::new(0, 35),
                },
                vec![0, 0, 35],
            ),
            (
                Message::Snapshot {
                    revision: 0,
                    windows: vec![],
                    workspaces: vec![],
                    locked: true,
                },
                vec![1, 0, 0, 0, 1],
            ),
            (
                Message::CommandResult {
                    id: 1,
                    status: crate::CommandStatus::Applied,
                },
                vec![4, 1, 0],
            ),
            (
                Message::Command {
                    id: 1,
                    kind: CommandKind::SetWellbeingEffects {
                        grayscale: false,
                        fade: true,
                    },
                },
                vec![3, 1, 19, 0, 1],
            ),
            (
                Message::KeyboardAidToggled {
                    aid: crate::KeyboardAid::StickyKeys,
                    enabled: true,
                },
                vec![17, 0, 1],
            ),
            (
                Message::Command {
                    id: 1,
                    kind: CommandKind::AuthStart {
                        method: Method::SessionStack,
                    },
                },
                vec![3, 1, 20, 0],
            ),
            (
                Message::AuthState {
                    start_request: 1,
                    key: None,
                    phase: Phase::Denied,
                },
                vec![18, 1, 0, 4],
            ),
        ] {
            let mut golden = (body.len() as u32).to_le_bytes().to_vec();
            golden.extend(body);
            assert_eq!(encode_frame(&message), golden);
            assert_eq!(decode_frame(&golden).unwrap(), message);
        }
    }
}
