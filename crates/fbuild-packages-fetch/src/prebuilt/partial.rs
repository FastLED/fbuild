//! Metadata shared by daemon transfers and synchronous startup/partial GC.
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
#[derive(Serialize, Deserialize)]
pub struct Meta {
    pub url: String,
    pub etag: Option<String>,
    pub owner: String,
    pub progress: u64,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
