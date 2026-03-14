//! Serial port management for fbuild.
//!
//! This is the most critical crate in the Rust port. It provides:
//!
//! - `SharedSerialManager`: Centralized serial I/O with broadcast readers / exclusive writer
//! - Deploy preemption protocol: force-close → flash → reconnect
//! - WebSocket message types for serial monitor API
//! - Windows USB-CDC quirks: re-enumeration retries, aggressive buffer draining
//! - Auto-reconnect after device reset
//! - Crash decoder integration
//!
//! ## Architecture
//!
//! ```text
//! Multiple Clients (CLI, API, WebSocket)
//!         ↓↓↓
//!   SharedSerialManager (tokio::sync::RwLock + per-port Mutex)
//!         ↓
//!   One serialport handle per physical port
//!         ↓
//!   Background reader task (tokio::spawn, per port)
//!         ↓
//!   Broadcast to all attached readers (tokio::sync::broadcast)
//! ```
//!
//! ## Key Design Decisions
//!
//! 1. All serial access routes through the daemon — no direct OS port locks
//! 2. Multiple readers (broadcast), exclusive writer (Mutex-gated)
//! 3. Deploy preemption forcibly closes sessions, notifies monitors via WebSocket
//! 4. Windows USB-CDC needs 30 retries with exponential backoff after hard reset

pub mod crash_decoder;
pub mod manager;
pub mod messages;
pub mod preemption;
pub mod session;

pub use manager::SharedSerialManager;
pub use messages::{SerialClientMessage, SerialServerMessage};
pub use session::SerialSession;
