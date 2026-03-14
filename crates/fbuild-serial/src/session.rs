//! Per-port serial session state.

use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;

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
    /// Client that opened the port.
    pub owner_client_id: Option<String>,
    /// Path to firmware ELF for crash decoding.
    pub elf_path: Option<PathBuf>,
}

impl SerialSession {
    pub fn new(port: String, baud_rate: u32) -> Self {
        Self {
            port,
            baud_rate,
            is_open: false,
            writer_client_id: None,
            reader_client_ids: HashSet::new(),
            output_buffer: VecDeque::with_capacity(10_000),
            total_bytes_read: 0,
            total_bytes_written: 0,
            started_at: 0.0,
            owner_client_id: None,
            elf_path: None,
        }
    }
}
