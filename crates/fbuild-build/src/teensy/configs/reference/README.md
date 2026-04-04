# PlatformIO Reference Configs

These JSON files capture the **authoritative linker flags** that PlatformIO uses
for each Teensy board. They were extracted from PlatformIO's SCons builder
environment and serve as the ground truth for validating fbuild's MCU configs.

## How to regenerate

Run the extraction script with PlatformIO installed:

```bash
uv run python ci/extract_pio_linker_flags.py --board teensy36 --board teensy41 --board teensylc
```

Or for all boards at once:

```bash
uv run python ci/extract_pio_linker_flags.py --all
```

## How tests use these

The Rust test `test_linker_flags_match_platformio_reference` in `mcu_config.rs`
loads both the fbuild MCU config and the corresponding reference file, then
validates that every linker flag from the reference is present in fbuild's config.

If a test fails, it means fbuild's MCU config has drifted from PlatformIO.
Either update the MCU config to include the missing flag, or regenerate the
reference if PlatformIO has changed its defaults.
