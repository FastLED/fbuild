# NXP LPC8xx MCU config

`nxplpc.json` is loaded by `super::mcu_config::get_nxplpc_config` and bakes
the Cortex-M0+ compiler / linker flags shared by every LPC8xx target. Board
JSON entries (lpc804.json, lpc845.json) supply the chip-specific defines via
`extra_flags`.
