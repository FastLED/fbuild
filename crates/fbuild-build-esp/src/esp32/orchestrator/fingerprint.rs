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
    pub board_chip_variant: Option<String>,
    pub board_extra_flags: Option<String>,
    pub board_upload_protocol: Option<String>,
    pub board_upload_speed: Option<String>,
    pub board_partitions: Option<String>,
    pub board_ldscript: Option<String>,
    pub board_platform: Option<String>,
    pub architecture: String,
    pub platform: String,
    pub toolchain_name: String,
    pub toolchain_version: String,
    pub flash_mode: String,
    pub flash_freq: String,
    pub flash_size: String,
    pub max_flash: Option<u64>,
    pub max_ram: Option<u64>,
    pub eh_frame_policy: &'static str,
}

#[cfg(test)]
mod tests {
    use super::Esp32FingerprintMetadata;

    fn metadata(toolchain_name: &str, toolchain_version: &str) -> Esp32FingerprintMetadata {
        Esp32FingerprintMetadata {
            version: 1,
            env_name: "esp32s3".into(),
            profile: "release".into(),
            board_name: "ESP32-S3".into(),
            board_mcu: "esp32s3".into(),
            board_define: "ESP32_S3_DEVKITC_1".into(),
            board_core: "esp32".into(),
            board_variant: "esp32s3".into(),
            board_variant_h: None,
            board_chip_variant: None,
            board_extra_flags: None,
            board_upload_protocol: None,
            board_upload_speed: None,
            board_partitions: None,
            board_ldscript: None,
            board_platform: None,
            architecture: "xtensa".into(),
            platform: "espressif32".into(),
            flash_mode: "dio".into(),
            flash_freq: "80m".into(),
            flash_size: "8MB".into(),
            max_flash: None,
            max_ram: None,
            eh_frame_policy: "preserve",
            toolchain_name: toolchain_name.into(),
            toolchain_version: toolchain_version.into(),
        }
    }

    #[test]
    fn selected_toolchain_changes_fast_path_metadata() {
        let original =
            serde_json::to_vec(&metadata("toolchain-xtensa-esp32s3", "8.4.0+2021r2-patch5"))
                .unwrap();
        let different_name =
            serde_json::to_vec(&metadata("toolchain-xtensa-esp32s2", "8.4.0+2021r2-patch5"))
                .unwrap();
        let different_version =
            serde_json::to_vec(&metadata("toolchain-xtensa-esp32s3", "14.2.0")).unwrap();
        assert_ne!(original, different_name);
        assert_ne!(original, different_version);
    }
}
