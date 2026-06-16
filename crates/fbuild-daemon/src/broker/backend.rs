//! running-process backend endpoint served by `fbuild-daemon`.
//!
//! The broker launches this daemon with `RUNNING_PROCESS_BROKER_V1_BACKEND_PIPE`
//! set. Binding that local-socket endpoint lets the broker verify the spawned
//! process and lets clients issue small broker-framed diagnostics before the
//! full build/deploy HTTP surface migrates.

use std::io::{Read, Write};
use std::sync::Arc;

use interprocess::local_socket::prelude::*;
use prost::Message;
use running_process::broker::backend_lifecycle::probe::{
    endpoint_probe_request_from_frame, endpoint_probe_response_frame,
};
use running_process::broker::backend_lifecycle::DaemonProcess;
use running_process::broker::protocol::{
    read_frame, write_frame, Endpoint, Frame, FrameKind, FramingError,
};
use running_process::broker::server::backend_launcher::{
    BACKEND_ENV_ENDPOINT_NAMESPACE, BACKEND_ENV_ENDPOINT_PATH,
};

use crate::broker::protocol::{BrokerRequest, BrokerResponse, DaemonOp, FBUILD_PAYLOAD_PROTOCOL};
use crate::context::DaemonContext;
use crate::models::{DaemonInfoResponse, HealthResponse};

/// Start the broker backend endpoint if this process was broker-launched.
pub fn spawn_backend_endpoint_if_requested(ctx: Arc<DaemonContext>) {
    let Some(endpoint) = broker_backend_endpoint_from_env() else {
        return;
    };

    if let Err(err) = std::thread::Builder::new()
        .name("fbuild-rp-backend".to_string())
        .spawn(move || {
            if let Err(err) = serve_backend_endpoint(endpoint, ctx) {
                tracing::warn!("running-process backend endpoint exited: {err}");
            }
        })
    {
        tracing::warn!("failed to spawn running-process backend endpoint thread: {err}");
    }
}

fn broker_backend_endpoint_from_env() -> Option<Endpoint> {
    let path = std::env::var(BACKEND_ENV_ENDPOINT_PATH).ok()?;
    if path.is_empty() {
        return None;
    }
    let namespace_id = std::env::var(BACKEND_ENV_ENDPOINT_NAMESPACE)
        .unwrap_or_else(|_| fbuild_paths::running_process::BROKER_ISOLATION.to_string());
    Some(Endpoint { namespace_id, path })
}

fn serve_backend_endpoint(
    endpoint: Endpoint,
    ctx: Arc<DaemonContext>,
) -> Result<(), BackendEndpointError> {
    let daemon = DaemonProcess::current_process(endpoint.clone(), Some(30))?;
    let listener = bind_local_socket(&endpoint.path)?;
    tracing::info!("serving running-process backend endpoint {}", endpoint.path);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let ctx = ctx.clone();
                let daemon = daemon.clone();
                std::thread::spawn(move || {
                    if let Err(err) = handle_backend_connection(stream, &daemon, &ctx) {
                        tracing::debug!("running-process backend connection failed: {err}");
                    }
                });
            }
            Err(err) => tracing::debug!("running-process backend accept failed: {err}"),
        }
    }
    Ok(())
}

fn handle_backend_connection<S>(
    mut stream: S,
    daemon: &DaemonProcess,
    ctx: &Arc<DaemonContext>,
) -> Result<(), BackendEndpointError>
where
    S: Read + Write,
{
    let frame_bytes = read_frame(&mut stream)?;
    let frame = Frame::decode(frame_bytes.as_slice())?;

    if frame.payload_protocol
        == running_process::broker::protocol::registry::BACKEND_HANDLE_PROBE_PAYLOAD_PROTOCOL
    {
        let request = endpoint_probe_request_from_frame(&frame)?;
        let reply = endpoint_probe_response_frame(&request, daemon);
        write_frame_bytes(&mut stream, &reply)?;
        return Ok(());
    }

    if frame.payload_protocol != FBUILD_PAYLOAD_PROTOCOL {
        return Err(BackendEndpointError::UnexpectedPayloadProtocol(
            frame.payload_protocol,
        ));
    }
    if FrameKind::try_from(frame.kind) != Ok(FrameKind::Request) {
        return Err(BackendEndpointError::UnexpectedFrameKind(frame.kind));
    }

    let request = BrokerRequest::from_prost_bytes(&frame.payload)?;
    let response = dispatch_broker_request(request, ctx);
    let response_frame = Frame::response_to(&frame, response.to_prost_bytes());
    write_frame_bytes(&mut stream, &response_frame)?;
    Ok(())
}

fn dispatch_broker_request(req: BrokerRequest, ctx: &Arc<DaemonContext>) -> BrokerResponse {
    match req.op {
        DaemonOp::Health => json_response(health_response(ctx)),
        DaemonOp::DaemonInfo => json_response(daemon_info_response(ctx)),
        other => BrokerResponse::err(format!(
            "{other:?} is not served over the broker frame path yet; use the HTTP fallback"
        )),
    }
}

fn json_response<T: serde::Serialize>(value: T) -> BrokerResponse {
    match serde_json::to_string(&value) {
        Ok(json) => BrokerResponse::ok(json),
        Err(err) => BrokerResponse::err(format!("failed to serialize broker response: {err}")),
    }
}

fn health_response(ctx: &DaemonContext) -> HealthResponse {
    ctx.touch_activity();
    HealthResponse {
        status: "healthy".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        source_mtime: ctx.source_mtime,
    }
}

fn daemon_info_response(ctx: &DaemonContext) -> DaemonInfoResponse {
    use std::sync::atomic::Ordering;

    ctx.touch_activity();
    let daemon_state = *ctx.daemon_state.read().unwrap_or_else(|e| e.into_inner());
    let current_operation = ctx
        .current_operation
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let cache_identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    DaemonInfoResponse {
        status: "running".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        port: ctx.port,
        started_at: ctx.started_at_unix,
        dev_mode: fbuild_paths::is_dev_mode(),
        host: "127.0.0.1".to_string(),
        operation_in_progress: ctx.operation_in_progress.load(Ordering::Relaxed),
        daemon_state,
        current_operation,
        dependency_install: ctx.dependency_install_snapshot(),
        client_count: ctx.serial_manager.get_port_sessions().len(),
        cache_dir: cache_identity.cache_root.to_string_lossy().to_string(),
        cache_identity: cache_identity.label_value(),
        cache_schema_version: fbuild_paths::running_process::CACHE_SCHEMA_VERSION,
        daemon_dir: fbuild_paths::get_daemon_dir().to_string_lossy().to_string(),
        source_mtime: ctx.source_mtime,
        spawner_cwd: ctx.spawner_cwd.clone(),
        mcp_url: format!("http://127.0.0.1:{}/mcp", ctx.port),
        watch_set_cache: Some(ctx.watch_set_cache.stats()),
    }
}

fn write_frame_bytes<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), BackendEndpointError> {
    let mut body = Vec::new();
    frame.encode(&mut body)?;
    write_frame(writer, &body)?;
    Ok(())
}

fn bind_local_socket(path: &str) -> Result<interprocess::local_socket::Listener, std::io::Error> {
    #[cfg(unix)]
    {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(path);
        use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
        let name = path.to_fs_name::<GenericFilePath>()?;
        ListenerOptions::new().name(name).create_sync()
    }

    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName};
        let name = path.to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new().name(name).create_sync()
    }
}

#[derive(Debug, thiserror::Error)]
enum BackendEndpointError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Identity(#[from] running_process::broker::backend_lifecycle::identity::IdentityError),
    #[error(transparent)]
    Framing(#[from] FramingError),
    #[error(transparent)]
    DecodeFrame(#[from] prost::DecodeError),
    #[error(transparent)]
    EncodeFrame(#[from] prost::EncodeError),
    #[error(transparent)]
    Probe(#[from] running_process::broker::backend_lifecycle::probe::EndpointProbeServerError),
    #[error(transparent)]
    Protocol(#[from] crate::broker::protocol::ProtocolError),
    #[error("unexpected payload protocol {0:#06X}")]
    UnexpectedPayloadProtocol(u32),
    #[error("unexpected frame kind {0}")]
    UnexpectedFrameKind(i32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use running_process::broker::backend_lifecycle::probe::PROBE_NONCE_BYTES;
    use running_process::broker::protocol::registry::BACKEND_HANDLE_PROBE_PAYLOAD_PROTOCOL;
    use serde_json::Value;
    use std::io::{Cursor, Result as IoResult};

    struct Duplex {
        read: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Duplex {
        fn new(input: Vec<u8>) -> Self {
            Self {
                read: Cursor::new(input),
                written: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
            self.read.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    fn encode_frame(frame: &Frame) -> Vec<u8> {
        let mut body = Vec::new();
        frame.encode(&mut body).unwrap();
        let mut framed = Vec::new();
        write_frame(&mut framed, &body).unwrap();
        framed
    }

    fn decode_written(stream: &mut Duplex) -> Frame {
        let body = read_frame(&mut Cursor::new(std::mem::take(&mut stream.written))).unwrap();
        Frame::decode(body.as_slice()).unwrap()
    }

    fn daemon_process() -> DaemonProcess {
        DaemonProcess::current_process(
            Endpoint {
                namespace_id: "test".to_string(),
                path: "test-backend".to_string(),
            },
            Some(30),
        )
        .unwrap()
    }

    fn daemon_context() -> Arc<DaemonContext> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(8765, shutdown_tx, "test".to_string()))
    }

    #[test]
    fn replies_to_backend_probe_frames() {
        let nonce = [1_u8; PROBE_NONCE_BYTES];
        let request = Frame::request(BACKEND_HANDLE_PROBE_PAYLOAD_PROTOCOL, nonce.to_vec())
            .with_request_id(7);
        let mut stream = Duplex::new(encode_frame(&request));
        let daemon = daemon_process();
        let ctx = daemon_context();

        handle_backend_connection(&mut stream, &daemon, &ctx).unwrap();

        let response = decode_written(&mut stream);
        assert_eq!(
            response.payload_protocol,
            BACKEND_HANDLE_PROBE_PAYLOAD_PROTOCOL
        );
        assert_eq!(response.request_id, 7);
        assert!(response.payload.starts_with(&nonce));
    }

    #[test]
    fn serves_health_over_broker_frames() {
        let request = BrokerRequest::new(DaemonOp::Health, "{}");
        let request_frame =
            Frame::request(FBUILD_PAYLOAD_PROTOCOL, request.to_prost_bytes()).with_request_id(9);
        let mut stream = Duplex::new(encode_frame(&request_frame));
        let daemon = daemon_process();
        let ctx = daemon_context();

        handle_backend_connection(&mut stream, &daemon, &ctx).unwrap();

        let response_frame = decode_written(&mut stream);
        assert_eq!(response_frame.payload_protocol, FBUILD_PAYLOAD_PROTOCOL);
        assert_eq!(response_frame.request_id, 9);
        let response = BrokerResponse::from_prost_bytes(&response_frame.payload).unwrap();
        assert!(
            response.success,
            "expected successful health response, got: {:?}",
            response
        );
        let payload: Value = serde_json::from_str(&response.payload_json).unwrap();
        assert_eq!(payload["status"], "healthy");
        assert_eq!(payload["port"], Value::Null);
    }
}
