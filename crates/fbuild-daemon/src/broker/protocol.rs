//! fbuild's registered v1 broker payload protocol and the single internal
//! request/response model shared by the direct and broker paths.
//!
//! # Encoding lane (JSON)
//!
//! The inventory (`docs/running-process/inventory.md`) found that
//! `fbuild-daemon` already speaks **JSON over loopback HTTP**. Per the
//! adoption guide's encoding decision table, the JSON lane is:
//!
//! > keep JSON direct path, add prost broker path, run parity tests for both
//! > encodings.
//!
//! So the internal model below ([`BrokerRequest`] / [`BrokerResponse`]) is the
//! single source of truth. It serializes to:
//!
//! - **JSON** (`serde`) on the legacy direct loopback-HTTP path, preserving the
//!   exact request/response bodies `DaemonClient` already sends; and
//! - **prost** ([`FbuildRequest`] / [`FbuildResponse`]) on the v1 broker
//!   `Frame` lane.
//!
//! The prost envelope deliberately carries the operation discriminator plus the
//! JSON-encoded operation payload (`payload_json`) verbatim. fbuild's HTTP
//! handlers already accept/emit that JSON, so the daemon keeps **one** body
//! parser for both transports — the broker path is a pure framing change, not a
//! schema fork. Golden-message parity tests assert the two encodings of the
//! same [`BrokerRequest`]/[`BrokerResponse`] stay in lock-step.

use prost::Message;
use serde::{Deserialize, Serialize};

// fbuild's registered consumer payload-protocol ID for the v1 broker `Frame`
// lane. Authoritatively registered in running-process
// (`broker::protocol::registry`, zackees/running-process#437). The
// `register_payload_protocol!` macro re-pins it here with compile-time range +
// first-party-collision checks so the two sides can never drift.
running_process::register_payload_protocol! {
    /// fbuild's opaque Frame v1 request/response lane (registered consumer ID
    /// `0x7EB1`; see `crates/fbuild-daemon/src/broker/README.md`). The plain
    /// display copy lives in `fbuild_paths::running_process::FBUILD_PAYLOAD_PROTOCOL`.
    pub const FBUILD_PAYLOAD_PROTOCOL: u32 = 0x7EB1;
}

/// fbuild's internal request/response protocol version.
///
/// Bumped independently of the running-process broker `PROTOCOL_VERSION` (the
/// v1 envelope). This versions the *fbuild payload* schema so the daemon can
/// dual-read across a transition window if the internal model ever changes.
pub const FBUILD_PROTOCOL_VERSION: u32 = fbuild_paths::running_process::FBUILD_PROTOCOL_VERSION;

/// The daemon control operations fbuild multiplexes over a single
/// request/response model.
///
/// These mirror the existing `fbuild-daemon` HTTP endpoints (see
/// `crates/fbuild-cli/src/daemon_client.rs`). The numeric discriminants are the
/// frozen wire values — never renumber an existing variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i32)]
pub enum DaemonOp {
    /// `POST /api/build`
    Build = 0,
    /// `POST /api/deploy`
    Deploy = 1,
    /// `POST /api/monitor`
    Monitor = 2,
    /// `POST /api/test-emu`
    TestEmu = 3,
    /// `GET /api/daemon/info`
    DaemonInfo = 4,
    /// `GET /api/cache/stats`
    CacheStats = 5,
    /// `POST /api/cache/gc`
    Gc = 6,
    /// `GET /api/locks/status`
    LockStatus = 7,
    /// `GET /health`
    Health = 8,
}

impl DaemonOp {
    /// Stable wire discriminant.
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Decode a wire discriminant. Unknown values are rejected so a newer
    /// daemon talking to an older client surfaces a typed error instead of
    /// silently mis-dispatching.
    pub fn from_i32(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Build,
            1 => Self::Deploy,
            2 => Self::Monitor,
            3 => Self::TestEmu,
            4 => Self::DaemonInfo,
            5 => Self::CacheStats,
            6 => Self::Gc,
            7 => Self::LockStatus,
            8 => Self::Health,
            _ => return None,
        })
    }

    /// The HTTP path the direct loopback-HTTP path uses for this op.
    pub fn http_path(self) -> &'static str {
        match self {
            Self::Build => "/api/build",
            Self::Deploy => "/api/deploy",
            Self::Monitor => "/api/monitor",
            Self::TestEmu => "/api/test-emu",
            Self::DaemonInfo => "/api/daemon/info",
            Self::CacheStats => "/api/cache/stats",
            Self::Gc => "/api/cache/gc",
            Self::LockStatus => "/api/locks/status",
            Self::Health => "/health",
        }
    }
}

/// The single internal request model used by both the direct and broker paths.
///
/// `payload_json` is the exact JSON body `fbuild-daemon` already accepts for the
/// chosen [`DaemonOp`] (e.g. a serialized `BuildRequest`). The broker path wraps
/// this in [`FbuildRequest`]; the direct path POSTs `payload_json` to
/// `op.http_path()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerRequest {
    /// fbuild payload schema version.
    pub protocol_version: u32,
    /// Which daemon operation this request invokes.
    pub op: DaemonOp,
    /// The operation's JSON body (the existing direct-path request body).
    pub payload_json: String,
    /// Optional caller-supplied request id for correlation/auditing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl BrokerRequest {
    /// Build a request at the current protocol version.
    pub fn new(op: DaemonOp, payload_json: impl Into<String>) -> Self {
        Self {
            protocol_version: FBUILD_PROTOCOL_VERSION,
            op,
            payload_json: payload_json.into(),
            request_id: None,
        }
    }

    /// Attach a correlation id.
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Encode for the **direct** path (the JSON body POSTed to `op.http_path()`).
    pub fn to_json(&self) -> Result<String, serde_json::error::Error> {
        serde_json::to_string(self)
    }

    /// Encode for the **broker** path (prost over the v1 `Frame` lane).
    pub fn to_prost_bytes(&self) -> Vec<u8> {
        FbuildRequest::from(self).encode_to_vec()
    }

    /// Decode the broker-path prost bytes back into the internal model.
    pub fn from_prost_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let wire = FbuildRequest::decode(bytes)?;
        Self::try_from(wire)
    }
}

/// The single internal response model used by both the direct and broker paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerResponse {
    /// fbuild payload schema version.
    pub protocol_version: u32,
    /// Whether the operation succeeded.
    pub success: bool,
    /// The operation's JSON response body (the existing direct-path body).
    pub payload_json: String,
    /// Structured error envelope when `success == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BrokerResponse {
    /// A successful response carrying the operation's JSON result body.
    pub fn ok(payload_json: impl Into<String>) -> Self {
        Self {
            protocol_version: FBUILD_PROTOCOL_VERSION,
            success: true,
            payload_json: payload_json.into(),
            error: None,
        }
    }

    /// An error response (the daemon's structured error envelope).
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            protocol_version: FBUILD_PROTOCOL_VERSION,
            success: false,
            payload_json: String::new(),
            error: Some(message.into()),
        }
    }

    /// Encode for the direct path (JSON).
    pub fn to_json(&self) -> Result<String, serde_json::error::Error> {
        serde_json::to_string(self)
    }

    /// Encode for the broker path (prost).
    pub fn to_prost_bytes(&self) -> Vec<u8> {
        FbuildResponse::from(self).encode_to_vec()
    }

    /// Decode the broker-path prost bytes back into the internal model.
    pub fn from_prost_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let wire = FbuildResponse::decode(bytes)?;
        Self::try_from(wire)
    }
}

/// Errors decoding the broker-path prost wire into the internal model.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The prost frame could not be decoded.
    #[error("prost decode: {0}")]
    Decode(#[from] prost::DecodeError),
    /// The wire carried an operation discriminant this build does not know.
    #[error("unknown daemon op discriminant: {0}")]
    UnknownOp(i32),
}

// ---------------------------------------------------------------------------
// prost service-payload messages (the v1 broker Frame lane wire types).
//
// Hand-derived `#[derive(prost::Message)]` — no .proto/build.rs needed. Field
// numbers are FROZEN: never renumber an existing field; only append.
// ---------------------------------------------------------------------------

/// prost wire form of [`BrokerRequest`] (the v1 broker Frame payload).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FbuildRequest {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    #[prost(int32, tag = "2")]
    pub op: i32,
    #[prost(string, tag = "3")]
    pub payload_json: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "4")]
    pub request_id: ::core::option::Option<::prost::alloc::string::String>,
}

/// prost wire form of [`BrokerResponse`] (the v1 broker Frame payload).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FbuildResponse {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    #[prost(bool, tag = "2")]
    pub success: bool,
    #[prost(string, tag = "3")]
    pub payload_json: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "4")]
    pub error: ::core::option::Option<::prost::alloc::string::String>,
}

impl From<&BrokerRequest> for FbuildRequest {
    fn from(req: &BrokerRequest) -> Self {
        Self {
            protocol_version: req.protocol_version,
            op: req.op.as_i32(),
            payload_json: req.payload_json.clone(),
            request_id: req.request_id.clone(),
        }
    }
}

impl TryFrom<FbuildRequest> for BrokerRequest {
    type Error = ProtocolError;
    fn try_from(wire: FbuildRequest) -> Result<Self, Self::Error> {
        let op = DaemonOp::from_i32(wire.op).ok_or(ProtocolError::UnknownOp(wire.op))?;
        Ok(Self {
            protocol_version: wire.protocol_version,
            op,
            payload_json: wire.payload_json,
            request_id: wire.request_id,
        })
    }
}

impl From<&BrokerResponse> for FbuildResponse {
    fn from(resp: &BrokerResponse) -> Self {
        Self {
            protocol_version: resp.protocol_version,
            success: resp.success,
            payload_json: resp.payload_json.clone(),
            error: resp.error.clone(),
        }
    }
}

impl TryFrom<FbuildResponse> for BrokerResponse {
    type Error = ProtocolError;
    fn try_from(wire: FbuildResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            protocol_version: wire.protocol_version,
            success: wire.success,
            payload_json: wire.payload_json,
            error: wire.error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_protocol_is_registered_id() {
        // Pinned to the running-process consumer registry value (0x7EB1). The
        // `register_payload_protocol!` macro already const-asserts at compile
        // time that this lies in the registered-consumer range (0x7000..=0x7EFF)
        // and does not collide with any first-party running-process ID, so the
        // local constant is the authoritative pin on the consumer side.
        assert_eq!(FBUILD_PAYLOAD_PROTOCOL, 0x7EB1);
    }

    #[test]
    fn payload_protocol_matches_paths_display_copy() {
        // The CLI diagnostic prints the dependency-free copy in `fbuild-paths`;
        // it must never drift from the authoritative macro pin here.
        assert_eq!(
            FBUILD_PAYLOAD_PROTOCOL,
            fbuild_paths::running_process::FBUILD_PAYLOAD_PROTOCOL
        );
    }

    #[test]
    fn daemon_op_roundtrips_every_variant() {
        for op in [
            DaemonOp::Build,
            DaemonOp::Deploy,
            DaemonOp::Monitor,
            DaemonOp::TestEmu,
            DaemonOp::DaemonInfo,
            DaemonOp::CacheStats,
            DaemonOp::Gc,
            DaemonOp::LockStatus,
            DaemonOp::Health,
        ] {
            assert_eq!(DaemonOp::from_i32(op.as_i32()), Some(op));
            assert!(!op.http_path().is_empty());
        }
    }

    #[test]
    fn daemon_op_rejects_unknown_discriminant() {
        assert_eq!(DaemonOp::from_i32(9999), None);
    }

    /// Golden-message parity: the internal model must round-trip identically
    /// through BOTH the direct (JSON) and broker (prost) encodings.
    #[test]
    fn request_parity_json_and_prost() {
        let req = BrokerRequest::new(DaemonOp::Build, r#"{"project_dir":"/p","verbose":true}"#)
            .with_request_id("req-42");

        // Direct (JSON) round-trip.
        let json = req.to_json().expect("json encode");
        let from_json: BrokerRequest = serde_json::from_str(&json).expect("json decode");
        assert_eq!(from_json, req);

        // Broker (prost) round-trip.
        let bytes = req.to_prost_bytes();
        let from_prost = BrokerRequest::from_prost_bytes(&bytes).expect("prost decode");
        assert_eq!(from_prost, req);

        // Both encodings carry the same logical message.
        assert_eq!(from_json, from_prost);
    }

    #[test]
    fn response_parity_json_and_prost() {
        let resp = BrokerResponse::ok(r#"{"success":true,"exit_code":0}"#);
        let json = resp.to_json().expect("json encode");
        let from_json: BrokerResponse = serde_json::from_str(&json).expect("json decode");
        let bytes = resp.to_prost_bytes();
        let from_prost = BrokerResponse::from_prost_bytes(&bytes).expect("prost decode");
        assert_eq!(from_json, resp);
        assert_eq!(from_prost, resp);
        assert_eq!(from_json, from_prost);

        let err = BrokerResponse::err("daemon refused");
        assert!(!err.success);
        let err_round =
            BrokerResponse::from_prost_bytes(&err.to_prost_bytes()).expect("prost decode err");
        assert_eq!(err_round, err);
    }

    #[test]
    fn unknown_op_in_prost_is_typed_error() {
        let wire = FbuildRequest {
            protocol_version: FBUILD_PROTOCOL_VERSION,
            op: 4242,
            payload_json: String::new(),
            request_id: None,
        };
        let bytes = wire.encode_to_vec();
        match BrokerRequest::from_prost_bytes(&bytes) {
            Err(ProtocolError::UnknownOp(4242)) => {}
            other => panic!("expected UnknownOp(4242), got {other:?}"),
        }
    }
}
