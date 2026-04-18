//! HTTP/WebSocket daemon server for fbuild.
//!
//! Replaces the Python FastAPI daemon with an axum-based Rust server.
//! Maintains API compatibility: same endpoints, same JSON schemas.
//!
//! ## Endpoints
//!
//! Operations:
//! - POST /api/build
//! - POST /api/deploy
//! - POST /api/monitor
//! - POST /api/test-emu
//!
//! Management:
//! - GET  /health
//! - GET  /api/daemon/info
//! - POST /api/daemon/shutdown
//!
//! Devices:
//! - POST /api/devices/list
//! - GET  /api/devices/{port}/status
//! - POST /api/devices/{port}/lease
//! - POST /api/devices/{port}/release
//! - POST /api/devices/{port}/preempt
//!
//! WebSocket:
//! - GET  /ws/serial-monitor
//! - GET  /ws/status
//! - GET  /ws/logs
//! - GET  /ws/monitor/{session_id}

pub mod context;
pub mod device_manager;
pub mod handlers;
pub mod log_layer;
pub mod models;
pub mod status_manager;
