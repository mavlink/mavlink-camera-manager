//! # MCM API v1
//!
//! ## Serde derive convention
//!
//! Types in this module follow an intentional serde derive asymmetry:
//!
//! - **Response-only** types derive `Serialize` only (e.g., `Control`, `ApiVideoSource`,
//!   `Info`, `OnvifDevice`).
//! - **Request-only** types derive `Deserialize` only (e.g., `RemoveStream`, `BlockSource`,
//!   `ThumbnailFileRequest`).
//! - **Bidirectional** types (used in both requests and responses) derive both `Serialize`
//!   and `Deserialize` (e.g., `VideoAndStreamInformation`, `StreamInformation`).
//!
//! This is by design: deriving only what is needed makes the API contract explicit and
//! prevents accidental misuse (e.g., deserializing a server-only response type from
//! untrusted input).

pub mod controls;
pub mod server;
pub mod signalling;
pub mod stream;
pub mod video;

#[cfg(test)]
mod serde_tests;
