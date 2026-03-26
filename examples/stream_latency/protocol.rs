//! Signaling protocol types for the MCM WebRTC signaling server.
//!
//! This is a standalone mirror of `src/lib/stream/webrtc/signalling_protocol.rs`,
//! without the `ts-rs` dependency so this example compiles without pulling in the
//! full library crate.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type PeerId = Uuid;
pub type SessionId = Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Protocol {
    #[serde(flatten)]
    pub message: Message,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Message {
    Question(Question),
    Answer(Answer),
    Negotiation(Negotiation),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Question {
    PeerId,
    AvailableStreams,
    StartSession(BindOffer),
    EndSession(EndSessionQuestion),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BindOffer {
    pub consumer_id: PeerId,
    pub producer_id: PeerId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BindAnswer {
    pub consumer_id: PeerId,
    pub producer_id: PeerId,
    pub session_id: SessionId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EndSessionQuestion {
    #[serde(flatten)]
    pub bind: BindAnswer,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Answer {
    PeerId(PeerIdAnswer),
    AvailableStreams(Vec<Stream>),
    StartSession(BindAnswer),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerIdAnswer {
    pub id: PeerId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Stream {
    pub id: PeerId,
    pub name: String,
    pub encode: Option<String>,
    pub height: Option<u32>,
    pub width: Option<u32>,
    pub interval: Option<String>,
    pub source: Option<String>,
    pub created: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Negotiation {
    MediaNegotiation(MediaNegotiation),
    IceNegotiation(IceNegotiation),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaNegotiation {
    #[serde(flatten)]
    pub bind: BindAnswer,
    pub sdp: RTCSessionDescription,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RTCSessionDescription {
    Answer(Sdp),
    Offer(Sdp),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sdp {
    pub sdp: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceNegotiation {
    #[serde(flatten)]
    pub bind: BindAnswer,
    pub ice: RTCIceCandidateInit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RTCIceCandidateInit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_m_line_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_fragment: Option<String>,
}

impl From<Message> for Protocol {
    fn from(message: Message) -> Self {
        Protocol { message }
    }
}

impl From<Question> for Protocol {
    fn from(question: Question) -> Self {
        Protocol {
            message: Message::Question(question),
        }
    }
}

impl From<MediaNegotiation> for Protocol {
    fn from(negotiation: MediaNegotiation) -> Self {
        Protocol {
            message: Message::Negotiation(Negotiation::MediaNegotiation(negotiation)),
        }
    }
}

impl From<IceNegotiation> for Protocol {
    fn from(negotiation: IceNegotiation) -> Self {
        Protocol {
            message: Message::Negotiation(Negotiation::IceNegotiation(negotiation)),
        }
    }
}
