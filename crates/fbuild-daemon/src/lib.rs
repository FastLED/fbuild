//! HTTP/WebSocket daemon server for fbuild.
//!
//! Replaces the Python FastAPI daemon with an axum-based Rust server.
//! Maintains API compatibility: same endpoints, same JSON schemas.
//!
//! ## Endpoints
//!
//! Operations:
//! - POST /api/operations/build
//! - POST /api/operations/deploy
//! - POST /api/operations/monitor
//! - POST /api/operations/install-deps
//! - GET  /api/operations/status/{request_id}
//!
//! Management:
//! - GET  /api/daemon/info
//! - GET  /api/daemon/health
//! - POST /api/daemon/shutdown
//!
//! Devices:
//! - POST /api/devices/list
//! - POST /api/devices/lease
//! - POST /api/devices/release
//!
//! WebSocket:
//! - GET  /ws/serial-monitor (serial monitor API)
//! - GET  /ws/status (build output streaming)
