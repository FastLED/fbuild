//! fbuild's v1 broker session adoption with the `RUNNING_PROCESS_DISABLE=1`
//! direct-path escape hatch and typed `Refused` handling.
//!
//! `fbuild-daemon` is a tokio runtime, so this uses the **async** broker
//! session (`AsyncBrokerSession::adopt`, gated on the `client-async` feature).
//! The Hello handshake sends `service_name = "fbuild"`, protocol min/max = 1,
//! `client_lib_name = "running-process"`, and `wanted_version = <fbuild daemon
//! version>`; the broker replies `Negotiated { backend_pipe, daemon_version }`
//! or a typed `Refused`.

use running_process::broker::adopt::{AdoptError, AsyncBrokerSession, OwnedConnectRequest};
use running_process::broker::client::RefusalKind;

use super::protocol::{BrokerRequest, BrokerResponse, ProtocolError, FBUILD_PAYLOAD_PROTOCOL};

/// Cap on broker negotiation / dial. An unhealthy broker that accepts
/// the connection but never replies to the negotiation frame used to
/// hang the entire daemon startup sequence here. See FastLED/fbuild#808.
const BROKER_ADOPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Cap on a single broker frame round-trip from this session. Without
/// this, any HTTP/WS handler that routes through the broker frame path
/// could be wedged by a non-responding broker. See FastLED/fbuild#808.
const BROKER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What `adopt` decided after consulting the escape hatch and the broker.
#[derive(Debug)]
pub enum AdoptOutcome {
    /// The broker negotiated a backend; use the returned session. Boxed because
    /// the session holds a sizeable async frame client and dwarfs the
    /// `UseDirectPath` variant.
    Negotiated(Box<FbuildBrokerSession>),
    /// `RUNNING_PROCESS_DISABLE=1` is set — the caller must use the legacy
    /// direct loopback-HTTP path (`DaemonClient`).
    UseDirectPath,
}

/// Errors talking to the broker (after the escape hatch has been honoured).
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    /// A typed broker refusal — `refusal_kind` classifies it.
    #[error("broker refused fbuild: {kind:?}")]
    Refused { kind: RefusalKind },
    /// Broker negotiation / backend dial failed for a non-refusal reason
    /// (broker unreachable, dial IO error, async worker join failure).
    #[error("broker connect failed: {0}")]
    Connect(String),
    /// A request frame round-trip failed.
    #[error("broker request failed: {0}")]
    Request(String),
    /// The response frame payload could not be decoded into the internal model.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// The disable env var held an invalid value.
    #[error("invalid RUNNING_PROCESS_DISABLE value: {0}")]
    DisableEnv(String),
}

/// An adopted fbuild broker session: a ready-to-talk frame lane to the
/// negotiated fbuild backend.
pub struct FbuildBrokerSession {
    inner: AsyncBrokerSession,
}

impl std::fmt::Debug for FbuildBrokerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FbuildBrokerSession")
            .field("endpoint", &self.inner.endpoint())
            .finish()
    }
}

impl FbuildBrokerSession {
    /// Adopt a broker session for fbuild.
    ///
    /// Honours `RUNNING_PROCESS_DISABLE=1` first (returns
    /// [`AdoptOutcome::UseDirectPath`] so the caller falls back to
    /// `DaemonClient`), then negotiates through the broker. `wanted_version` is
    /// the fbuild daemon/worker version (e.g. `env!("CARGO_PKG_VERSION")`).
    pub async fn adopt(
        broker_endpoint: impl Into<String>,
        wanted_version: impl Into<String>,
    ) -> Result<AdoptOutcome, BrokerError> {
        let request = OwnedConnectRequest::new(
            broker_endpoint,
            super::service::SERVICE_NAME,
            wanted_version,
            env!("CARGO_PKG_VERSION"),
        );

        // FastLED/fbuild#808: bound broker negotiation so an
        // unhealthy broker cannot wedge daemon startup.
        let adopt_result =
            match tokio::time::timeout(BROKER_ADOPT_TIMEOUT, AsyncBrokerSession::adopt(request))
                .await
            {
                Ok(r) => r,
                Err(_) => {
                    return Err(BrokerError::Connect(format!(
                        "broker negotiation timed out after {}s",
                        BROKER_ADOPT_TIMEOUT.as_secs()
                    )));
                }
            };
        match adopt_result {
            Ok(inner) => Ok(AdoptOutcome::Negotiated(Box::new(Self { inner }))),
            Err(AdoptError::BrokerDisabled) => Ok(AdoptOutcome::UseDirectPath),
            Err(AdoptError::DisableEnv(err)) => Err(BrokerError::DisableEnv(err.to_string())),
            Err(AdoptError::Connect(err)) => match err.refusal_kind() {
                // The broker spoke and declined — a typed setup error.
                Some(kind) => Err(BrokerError::Refused { kind }),
                // Not a refusal: a dial / IO / unreachable-broker failure.
                None => Err(BrokerError::Connect(err.to_string())),
            },
            Err(other) => Err(BrokerError::Connect(other.to_string())),
        }
    }

    /// The negotiated backend endpoint (`Negotiated.backend_pipe`), cacheable
    /// for a Hello-skip on the next adopt.
    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    /// The backend version the broker chose (`Negotiated.daemon_version`).
    pub fn daemon_version(&self) -> Option<&str> {
        self.inner.negotiated().map(|n| n.daemon_version.as_str())
    }

    /// Send one fbuild request over the broker frame lane and decode the
    /// response into the shared internal model.
    pub async fn request(&mut self, req: &BrokerRequest) -> Result<BrokerResponse, BrokerError> {
        // FastLED/fbuild#808: bound broker frame round-trips.
        let request_fut = self
            .inner
            .request(FBUILD_PAYLOAD_PROTOCOL, req.to_prost_bytes());
        let frame = match tokio::time::timeout(BROKER_REQUEST_TIMEOUT, request_fut).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(e)) => return Err(BrokerError::Request(e.to_string())),
            Err(_) => {
                return Err(BrokerError::Request(format!(
                    "broker request timed out after {}s",
                    BROKER_REQUEST_TIMEOUT.as_secs()
                )));
            }
        };
        Ok(BrokerResponse::from_prost_bytes(&frame.payload)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::protocol::DaemonOp;

    /// Both env-sensitive contracts in ONE test so the process-global
    /// `RUNNING_PROCESS_DISABLE` toggle can't race a sibling test running in
    /// parallel:
    ///
    /// 1. With `RUNNING_PROCESS_DISABLE=1`, adopt short-circuits to the direct
    ///    path WITHOUT dialing the broker (rollback / escape-hatch contract).
    /// 2. With it unset, adopt against a bogus endpoint surfaces a non-refusal
    ///    `Connect` error (not a panic, not a `Refused`).
    #[tokio::test]
    async fn disable_env_and_unreachable_broker_contracts() {
        // (1) escape hatch wins before any dial.
        std::env::set_var("RUNNING_PROCESS_DISABLE", "1");
        let disabled = FbuildBrokerSession::adopt("unused-endpoint", "2.2.27").await;
        match disabled {
            Ok(AdoptOutcome::UseDirectPath) => {}
            other => panic!("expected UseDirectPath under disable env, got {other:?}"),
        }

        // (2) hatch unset → a real (failing) dial against a bogus endpoint.
        std::env::remove_var("RUNNING_PROCESS_DISABLE");
        let endpoint = if cfg!(windows) {
            "fbuild-broker-test-does-not-exist"
        } else {
            "/tmp/fbuild-broker-test-does-not-exist.sock"
        };
        match FbuildBrokerSession::adopt(endpoint, "2.2.27").await {
            Err(BrokerError::Connect(_)) => {}
            other => panic!("expected Connect error for unreachable broker, got {other:?}"),
        }
    }

    /// A request encodes to fbuild's registered payload protocol on the wire.
    #[test]
    fn request_uses_registered_payload_protocol() {
        let req = BrokerRequest::new(DaemonOp::Health, "{}");
        // The bytes are the prost form; the lane ID is the registered constant.
        assert_eq!(FBUILD_PAYLOAD_PROTOCOL, 0x7EB1);
        assert!(!req.to_prost_bytes().is_empty());
    }
}
