//! Espflash progress -> `tracing` bridge for the native write path.

use espflash::target::ProgressCallbacks;

use crate::esp32::FlashRegion;

use super::transport::region_name;

/// Bridges espflash's [`ProgressCallbacks`] into `tracing` so the
/// daemon's existing log broadcaster picks up write progress without
/// any new API surface.
///
/// Logging-only by design: a richer WebSocket bridge (structured
/// progress frames on the deploy WS channel) is a follow-up — this
/// type is the single seam where that upgrade lands.
pub(super) struct LoggingProgressBridge {
    pub(super) port: String,
    pub(super) region: Option<FlashRegion>,
    pub(super) total: usize,
    pub(super) last_current: usize,
    /// Last percentage we logged at. Throttles the per-block `update`
    /// spam to one line per 10% of a region so the daemon log stream
    /// stays readable.
    pub(super) last_pct_logged: u8,
}

impl LoggingProgressBridge {
    pub(super) fn new(port: &str) -> Self {
        Self {
            port: port.to_string(),
            region: None,
            total: 0,
            last_current: 0,
            last_pct_logged: 0,
        }
    }

    pub(super) fn enter_region(&mut self, region: FlashRegion) {
        self.region = Some(region);
        self.total = 0;
        self.last_current = 0;
        self.last_pct_logged = 0;
    }

    pub(super) fn region_label(&self) -> &'static str {
        match self.region {
            Some(r) => region_name(r),
            None => "unknown",
        }
    }
}

impl ProgressCallbacks for LoggingProgressBridge {
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total;
        self.last_current = 0;
        self.last_pct_logged = 0;
        tracing::info!(
            port = %self.port,
            region = self.region_label(),
            addr = format!("0x{:x}", addr),
            total,
            "native write: begin region"
        );
    }

    fn update(&mut self, current: usize) {
        self.last_current = current;
        if self.total == 0 {
            return;
        }
        let pct = ((current as u64 * 100) / self.total as u64).min(100) as u8;
        // Emit a log line every 10% boundary so a 1 MB write produces
        // ~10 lines rather than hundreds.
        if pct >= self.last_pct_logged + 10 {
            self.last_pct_logged = pct - (pct % 10);
            tracing::info!(
                port = %self.port,
                region = self.region_label(),
                pct,
                current,
                total = self.total,
                "native write: progress"
            );
        }
    }

    fn verifying(&mut self) {
        tracing::debug!(
            port = %self.port,
            region = self.region_label(),
            "native write: verifying region (espflash internal)"
        );
    }

    fn finish(&mut self, skipped: bool) {
        tracing::info!(
            port = %self.port,
            region = self.region_label(),
            skipped,
            "native write: region complete"
        );
    }
}
