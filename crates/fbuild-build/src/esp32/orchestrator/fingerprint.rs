//! Esp32 fast-path fingerprint metadata struct (serialised via stable JSON hash).

use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Esp32FingerprintMetadata {
    pub version: u32,
    pub env_name: String,
    pub profile: String,
    pub board_name: String,
    pub board_mcu: String,
    pub board_define: String,
    pub board_core: String,
    pub board_variant: String,
    pub board_variant_h: Option<String>,
    pub board_extra_flags: Option<String>,
    pub board_upload_protocol: Option<String>,
    pub board_upload_speed: Option<String>,
    pub board_partitions: Option<String>,
    pub board_ldscript: Option<String>,
    pub board_platform: Option<String>,
    pub architecture: String,
    pub platform: String,
    pub flash_mode: String,
    pub flash_freq: String,
    pub flash_size: String,
    pub max_flash: Option<u64>,
    pub max_ram: Option<u64>,
    pub eh_frame_policy: &'static str,
}
