//! # meshcore-companion
//!
//! A pure-Rust client for the MeshCore companion-frame TCP
//! protocol, as spoken by `pymc_core`'s `CompanionFrameServer`
//! (and by USB / serial MeshCore companion devices, with a
//! transport adapter).
//!
//! This crate is intentionally standalone: it does not depend on
//! any other Supply Drop BBS crate. The protocol logic, frame
//! encoding/decoding, and connection state machine are usable by
//! any application that wants to talk to a MeshCore radio bridge,
//! BBS or otherwise.
//!
//! ## Boundaries
//!
//! - **Transport-agnostic at the connection layer.** The same
//!   protocol logic should work over TCP (CompanionFrameServer)
//!   or a serial / USB stream (raw companion device); the public
//!   API takes any `AsyncRead + AsyncWrite`.
//! - **No application semantics.** This crate handles framing,
//!   identity handshake, contact management, and message
//!   delivery — but interpreting messages as *BBS commands* is
//!   `bbs-mesh`'s job.
//! - **Property + fuzz tested.** Untrusted bytes from the network
//!   reach our parser here. The decoder gets `proptest`
//!   roundtrip tests and `cargo fuzz` targets when implementation
//!   begins. See [docs/PROTOCOL.md](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/PROTOCOL.md).

// Suppress missing-docs until this crate is ready to publish.
#![allow(missing_docs)]

pub mod client;
pub mod constants;
pub mod error;
pub mod frame;
pub mod types;

pub use client::{ClientConfig, ClientEvent, CompanionClient, SendError, SerialConfig};
pub use error::FrameDecodeError;
pub use frame::{decode_inbound, encode_outbound, strip_frame_header, InboundFrame, OutboundFrame};
pub use types::{
    BattAndStorage, ChannelInfo, ChannelMsg, Contact, ContactMsg, DeviceInfo, ExportedContact,
    LoginSuccess, SelfInfo, SentResult,
};
