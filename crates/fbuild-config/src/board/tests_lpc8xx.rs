//! Unit tests for ArduinoCore-LPC8xx board metadata.

use std::collections::HashMap;

use super::BoardConfig;

#[test]
fn test_lpc8xx_arduino_core_board_configs() {
    type BoardCase = (
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        u64,
        u64,
        &'static str,
        &'static str,
    );

    let cases: &[BoardCase] = &[
        (
            "lpc845brk",
            "lpc845",
            "LPC845BRK",
            "lpc845brk",
            65_536,
            16_384,
            "CPU_LPC845M301JBD48",
            "target/lpc84x.cfg",
        ),
        (
            "lpcxpresso804",
            "lpc804",
            "LPCXPRESSO804",
            "lpcxpresso804",
            32_768,
            4_096,
            "CPU_LPC804M101JDH24",
            "target/lpc8xx.cfg",
        ),
        (
            "lpcxpresso845max",
            "lpc845",
            "LPCXPRESSO845MAX",
            "lpcxpresso845max",
            65_536,
            16_384,
            "CPU_LPC845M301JBD64",
            "target/lpc84x.cfg",
        ),
    ];

    for (board_id, mcu, board_macro, variant, flash, ram, cpu_define, openocd_target) in cases {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new())
            .unwrap_or_else(|e| panic!("failed to load {board_id}: {e}"));
        assert_eq!(config.platform(), Some(fbuild_core::Platform::NxpLpc));
        assert_eq!(config.mcu, *mcu);
        assert_eq!(config.board, *board_macro);
        assert_eq!(config.core, "lpc8xx");
        assert_eq!(config.variant, *variant);
        assert_eq!(config.max_flash, Some(*flash));
        assert_eq!(config.max_ram, Some(*ram));
        assert_eq!(config.upload_protocol.as_deref(), Some("cmsis-dap"));
        assert_eq!(config.upload_speed.as_deref(), Some("1000"));
        assert_eq!(config.openocd_target.as_deref(), Some(*openocd_target));
        assert!(
            config
                .ldscript
                .as_deref()
                .is_some_and(|script| script.ends_with("_flash.ld"))
        );
        assert!(
            config
                .debug_tools
                .as_ref()
                .is_some_and(|tools| tools.get("cmsis-dap").is_some_and(|tool| tool.onboard))
        );

        let defines = config.get_defines();
        assert_eq!(
            defines.get(&format!("ARDUINO_{board_macro}")),
            Some(&"1".to_string())
        );
        assert_eq!(defines.get(*cpu_define), Some(&"1".to_string()));
    }
}
