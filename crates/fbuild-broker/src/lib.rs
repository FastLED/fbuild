//! v1 running-process broker adoption for fbuild.
//!
//! This crate implements the full v1 broker path for fbuild on top of the
//! frozen running-process broker API (zackees/running-process#433):
//!
//! - [`protocol`] pins fbuild's registered payload-protocol ID and defines the
//!   single internal request/response model used by **both** the legacy direct
//!   loopback-HTTP path and the broker path, plus the prost service-payload
//!   messages that carry that model over the v1 `Frame` envelope.
//! - [`service`] builds + installs the fbuild `ServiceDefinition`
//!   (`SHARED_BROKER` for per-user local builds, `EXPLICIT_INSTANCE`
//!   `"ci-trusted"` for CI trust groups) and publishes the `CacheManifest`
//!   (artifact / index / temp / log / lock / runtime / config roots).
//! - [`session`] adopts the broker session (`AsyncBrokerSession::adopt`) with
//!   typed [`RefusalKind`](running_process::broker::client::RefusalKind)
//!   handling and the `RUNNING_PROCESS_DISABLE=1` direct-path escape hatch.
//!
//! The inventory that motivated these choices is recorded in
//! `docs/running-process/inventory.md`.

pub mod protocol;
pub mod service;
pub mod session;

pub use protocol::{
    BrokerRequest, BrokerResponse, DaemonOp, FBUILD_PAYLOAD_PROTOCOL, FBUILD_PROTOCOL_VERSION,
};
pub use service::{
    fbuild_cache_manifest, fbuild_ci_service_definition, fbuild_service_definition, CacheRoots,
    ServiceError,
};
pub use session::{AdoptOutcome, BrokerError, FbuildBrokerSession};
