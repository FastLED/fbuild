# Arduino Mega 2560 Test Project

Minimal blink + serial sketch for ATmega2560 (Arduino Mega) build validation.

Referenced by `CLAUDE.md` and `README.md` as the `simavr` emulator example:

```bash
soldr cargo run -p fbuild-cli -- test-emu tests/platform/mega -e megaatmega2560 --emulator simavr
```

Tracked under FastLED/fbuild#389 as the representative classic AVR megaAVR
(8-bit AVR, 256 KiB flash, 8 KiB RAM) fixture. FastLED platform support for
the m2560 family lives in `src/platforms/avr/atmega/m2560/`.
