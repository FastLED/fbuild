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
//!
//! Management:
//! - GET  /health
//! - GET  /api/daemon/info
//! - POST /api/daemon/shutdown
//!
//! Devices:
//! - POST /api/devices/list
//!
//! WebSocket:
//! - GET  /ws/serial-monitor

pub mod context;
pub mod handlers;
pub mod models;
