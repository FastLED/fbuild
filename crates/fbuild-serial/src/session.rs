//! Per-port serial session state.

use crate::messages::SerialClientMetadata;
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// State for a single managed serial port.
pub struct SerialSession {
    pub port: String,
    pub baud_rate: u32,
    pub is_open: bool,
    /// Client with exclusive write access.
    pub writer_client_id: Option<String>,
    /// Clients receiving broadcast output.
    pub reader_client_ids: HashSet<String>,
    /// Circular output buffer (default 10k lines).
    pub output_buffer: VecDeque<String>,
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub started_at: f64,
    pub last_activity_at: f64,
    pub last_write_at: Option<f64>,
    /// Client that opened the port.
    pub owner_client_id: Option<String>,
    /// Best-effort metadata keyed by serial client id.
    pub client_metadata: HashMap<String, SerialClientMetadata>,
    /// Path to firmware ELF for crash decoding.
    pub elf_path: Option<PathBuf>,
    /// The underlying serial port handle.
    pub serial_handle: Option<Arc<Mutex<Box<dyn serialport::SerialPort>>>>,
    /// Background reader task handle.
    pub reader_handle: Option<JoinHandle<()>>,
    /// Flag to signal the background reader to stop.
    pub stop_flag: Arc<AtomicBool>,
}

impl SerialSession {
    pub fn new(port: String, baud_rate: u32) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            port,
            baud_rate,
            is_open: false,
            writer_client_id: None,
            reader_client_ids: HashSet::new(),
            output_buffer: VecDeque::with_capacity(10_000),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: now,
            last_activity_at: now,
            last_write_at: None,
            owner_client_id: None,
            client_metadata: HashMap::new(),
            elf_path: None,
            serial_handle: None,
            reader_handle: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}
