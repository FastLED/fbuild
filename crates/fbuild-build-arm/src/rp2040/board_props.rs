//! Arduino-Pico board properties and framework-specific variant selection.

use std::collections::HashMap;
use std::path::Path;

pub(super) fn rp_board_defines(
    board: &fbuild_config::BoardConfig,
    mcu: &crate::generic_arm::ArmMcuConfig,
) -> HashMap<String, String> {
    // MCU values are defaults; board/project flags are more specific. In
    // particular, USB identity aliases must not replace explicit USBD_* flags.
    let mut defines = mcu.defines_map();
    defines.extend(board.get_defines());
    defines
}

/// User variant overrides take precedence over framework metadata. The board
/// manifest may describe another core (e.g. Mbed), so use the selected Arduino
/// framework's variant when it is available.
pub(super) fn resolve_variant(
    declared: &str,
    props: &Option<HashMap<String, String>>,
    overrides: &HashMap<String, String>,
) -> String {
    if overrides.contains_key("variant") {
        return declared.to_owned();
    }
    props
        .as_ref()
        .and_then(|props| props.get("variant"))
        .filter(|variant| !variant.is_empty())
        .map_or_else(|| declared.to_owned(), Clone::clone)
}

pub(super) fn apply_rp_board_props(
    board_props: &Option<HashMap<String, String>>,
    framework_dir: &Path,
    defines: &mut HashMap<String, String>,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };
    for key in [
        "usbpwr",
        "usbstack_flags",
        "variantdefines",
        "led",
        "libpicowdefs",
        "wificc",
        "sdfatdefines",
    ] {
        if let Some(flags) = props.get(key) {
            let expanded =
                flags.replace("{runtime.platform.path}", &framework_dir.to_string_lossy());
            let tokens = fbuild_core::shell_split::split(&expanded);
            defines.extend(fbuild_core::compiler_flags::parse_define_flags(&tokens));
        }
    }
    if let Some(value) = props.get("usb_manufacturer") {
        defines.insert("USB_MANUFACTURER".to_string(), value.clone());
    }
    if let Some(value) = props.get("usb_product") {
        defines.insert("USB_PRODUCT".to_string(), value.clone());
    }
    for (property, define) in [
        ("flash_total", "PICO_FLASH_SIZE_BYTES"),
        ("fs_start", "FS_START"),
        ("fs_end", "FS_END"),
    ] {
        if let Some(value) = props.get(property) {
            defines.insert(define.to_string(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_usb_aliases_override_rp_mcu_defaults() {
        for id in ["pico", "rpipico2"] {
            let overrides = HashMap::from([(
                "extra_flags".to_owned(),
                "-DUSBD_VID=TEST_VENDOR -DUSBD_PID=TEST_PRODUCT".to_owned(),
            )]);
            let board = fbuild_config::BoardConfig::from_board_id(id, &overrides).unwrap();
            let mcu =
                super::super::mcu_config::get_rp2040_config_for_mcu(&board.mcu.to_lowercase())
                    .unwrap();
            let defines = rp_board_defines(&board, &mcu);
            assert_eq!(
                defines.get("USBD_VID").map(String::as_str),
                Some("TEST_VENDOR"),
                "{id}"
            );
            assert_eq!(
                defines.get("USBD_PID").map(String::as_str),
                Some("TEST_PRODUCT"),
                "{id}"
            );
            let defaults = fbuild_config::BoardConfig::from_board_id(id, &HashMap::new()).unwrap();
            let defines = rp_board_defines(&defaults, &mcu);
            assert_eq!(defines.get("USBD_VID").map(String::as_str), Some("USB_VID"));
            assert_eq!(defines.get("USBD_PID").map(String::as_str), Some("USB_PID"));
        }
    }

    #[test]
    fn resolves_framework_variant_and_preserves_explicit_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("boards.txt");
        std::fs::write(
            &path,
            "fixture.name=Test board\nfixture.build.variant=framework_variant\n",
        )
        .unwrap();
        let props = crate::arduino_props::load_board_props_with_default_menus(&path, "fixture");
        assert_eq!(
            resolve_variant("manifest_variant", &props, &HashMap::new()),
            "framework_variant"
        );
        let overrides = HashMap::from([("variant".to_owned(), "custom_variant".to_owned())]);
        assert_eq!(
            resolve_variant("custom_variant", &props, &overrides),
            "custom_variant"
        );
        assert_eq!(
            resolve_variant("manifest_variant", &None, &HashMap::new()),
            "manifest_variant"
        );
    }
}
