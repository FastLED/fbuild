use super::*;

#[test]
fn legacy_espressif32_1_11_2_uses_xtensa32() {
    // The official espressif32@1.11.2 manifest declares this sole Xtensa package.
    let packages = serde_json::json!({"toolchain-xtensa32": {"version": "~2.50200.0"}});
    assert_eq!(
        toolchain_name_for(&get_mcu_config("esp32").unwrap(), |name| packages
            .get(name)
            .is_some()),
        "toolchain-xtensa32"
    );
}
