# RP2040 MCU Configs

USB device macros `USBD_VID` and `USBD_PID` alias the registry-supplied
`USB_VID` and `USB_PID`; these configs contain no board USB identity values.

JSON configuration files for Raspberry Pi RP2040/RP2350 MCU families.

- `rp2040.json` - RP2040 (Cortex-M0+, no FPU)
- `rp2350.json` - RP2350 (Cortex-M33, optional FPU)
