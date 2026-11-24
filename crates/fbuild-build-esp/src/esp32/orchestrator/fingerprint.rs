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
    /// SDK `-D` flags reach every TU but come from the framework, not the
    /// project config, so a change there must not replay a stale fast path.
    pub sdk_defines: Vec<String>,
    /// Resolved `--shrink` mode. A reducer rewrites link inputs, so flipping
    /// `off` <-> `printf` must invalidate the fast path and relink
    /// (FastLED/fbuild#1459).
    pub shrink_mode: String,
    /// The framework term: the component identities of the installed framework
    /// (docs/design/artifact-serving-decisions.md §8a). Every other field here
    /// describes the board, the toolchain and the flags; this one covers the
    /// framework bytes those flags point at, so an SDK re-install or a compat
    /// patch that lands the same paths with different bytes cannot replay a
    /// stale fast path.
    ///
    /// Absent (a legacy, unstamped tree) is left out of the serialized metadata
    /// entirely, so those fast paths hash exactly as they did before this field
    /// existed; only a tree that carries identities gains the term.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework_identity: Option<String>,
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
            sdk_defines: vec!["-DESP_PLATFORM".into()],
            shrink_mode: "off".into(),
            framework_identity: None,
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

    #[test]
    fn shrink_mode_changes_fast_path_metadata() {
        let shrink_off = metadata("toolchain-xtensa-esp32s3", "14.2.0");
        let mut shrink_printf = metadata("toolchain-xtensa-esp32s3", "14.2.0");
        shrink_printf.shrink_mode = "printf".into();
        assert_ne!(
            serde_json::to_vec(&shrink_off).unwrap(),
            serde_json::to_vec(&shrink_printf).unwrap()
        );
    }

    #[test]
    fn framework_identity_changes_fast_path_metadata() {
        let legacy = metadata("toolchain-xtensa-esp32s3", "14.2.0");
        let mut stamped = metadata("toolchain-xtensa-esp32s3", "14.2.0");
        stamped.framework_identity = Some("0f0f".into());
        assert_ne!(
            serde_json::to_vec(&legacy).unwrap(),
            serde_json::to_vec(&stamped).unwrap()
        );

        let mut other_bytes = metadata("toolchain-xtensa-esp32s3", "14.2.0");
        other_bytes.framework_identity = Some("1e1e".into());
        assert_ne!(
            serde_json::to_vec(&stamped).unwrap(),
            serde_json::to_vec(&other_bytes).unwrap()
        );
    }

    #[test]
    fn an_unstamped_framework_leaves_the_metadata_unchanged() {
        // A legacy tree must hash exactly as it did before this field existed:
        // the term is skipped, not serialized as null.
        let json = serde_json::to_value(metadata("toolchain-xtensa-esp32s3", "14.2.0")).unwrap();
        assert!(json.get("framework_identity").is_none());
    }

    #[test]
    fn sdk_defines_change_fast_path_metadata() {
        let original = metadata("toolchain-xtensa-esp32s3", "8.4.0+2021r2-patch5");
        let mut without = metadata("toolchain-xtensa-esp32s3", "8.4.0+2021r2-patch5");
        without.sdk_defines.clear();
        assert_ne!(
            serde_json::to_vec(&original).unwrap(),
            serde_json::to_vec(&without).unwrap()
        );
    }
}
