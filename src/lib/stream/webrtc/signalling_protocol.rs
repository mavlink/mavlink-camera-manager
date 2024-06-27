use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Protocol {
    // version: String, // https://stackoverflow.com/a/70367795/3850957
    #[serde(flatten)]
    pub message: Message,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Message {
    Question(Question),
    Answer(Answer),
    Negotiation(Negotiation),
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Question {
    PeerId,
    AvailableStreams,
    StartSession(BindOffer),
    EndSession(EndSessionQuestion),
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct BindOffer {
    /// each tab in the browser
    pub consumer_id: PeerId,
    /// each stream/camera
    pub producer_id: PeerId,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct BindAnswer {
    /// each tab in the browser
    pub consumer_id: PeerId,
    /// each stream/camera
    pub producer_id: PeerId,
    /// associated session
    pub session_id: SessionId,
}

pub type PeerId = Uuid;

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct EndSessionQuestion {
    #[serde(flatten)]
    pub bind: BindAnswer,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Answer {
    PeerId(PeerIdAnswer),
    AvailableStreams(Vec<Stream>),
    StartSession(BindAnswer),
}

#[derive(Debug, Serialize, Deserialize, TS)]
pub struct PeerIdAnswer {
    pub id: PeerId,
}

#[derive(Debug, Serialize, Deserialize, TS)]
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

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "content", rename_all = "camelCase")]
pub enum Negotiation {
    MediaNegotiation(MediaNegotiation),
    IceNegotiation(IceNegotiation),
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MediaNegotiation {
    #[serde(flatten)]
    pub bind: BindAnswer,
    #[ts(type = "RTCSessionDescription")]
    pub sdp: RTCSessionDescription,
}

pub type SessionId = Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
/// [RTCSessionDescription](https://developer.mozilla.org/en-US/docs/Web/API/RTCSessionDescription) as expected by browsers.
/// Note: `pranswer` and `rollback` are not supported here.
pub enum RTCSessionDescription {
    /// Agreed-upon SDP
    Answer(Sdp),
    /// Initial/proposed SDP
    Offer(Sdp),
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Sdp {
    pub sdp: String,
}

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IceNegotiation {
    #[serde(flatten)]
    pub bind: BindAnswer,
    #[ts(type = "RTCIceCandidateInit")]
    pub ice: RTCIceCandidateInit,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
// [RTCIceCandidateInit](https://docs.w3cub.com/dom/rtcicecandidateinit) as expected by browsers.
pub struct RTCIceCandidateInit {
    /// The ICE candidate-attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    /// Associated media stream tag
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    /// Associated SDP index
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdp_m_line_index: Option<u32>,
    /// Producer's usernameFragment
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username_fragment: Option<String>,
}

impl From<Message> for Protocol {
    fn from(message: Message) -> Self {
        Protocol { message }
    }
}

impl From<Answer> for Protocol {
    fn from(answer: Answer) -> Self {
        Protocol {
            message: Message::Answer(answer),
        }
    }
}

impl From<Question> for Protocol {
    fn from(question: Question) -> Self {
        Protocol {
            message: Message::Question(question),
        }
    }
}

impl From<MediaNegotiation> for Message {
    fn from(negotiation: MediaNegotiation) -> Self {
        Message::Negotiation(Negotiation::MediaNegotiation(negotiation))
    }
}

impl From<IceNegotiation> for Message {
    fn from(negotiation: IceNegotiation) -> Self {
        Message::Negotiation(Negotiation::IceNegotiation(negotiation))
    }
}

impl From<Answer> for Message {
    fn from(answer: Answer) -> Self {
        Message::Answer(answer)
    }
}

impl From<Question> for Message {
    fn from(question: Question) -> Self {
        Message::Question(question)
    }
}
