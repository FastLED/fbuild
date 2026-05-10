# SuperMini nRF52840 Test Project

Minimal blink sketch for the SuperMini nRF52840 community board build validation.

The board reuses the `nrf52840_dk_adafruit` PlatformIO board because no first-party
PlatformIO board package exists for this community variant. The
`-DTARGET_SUPERMINI_NRF52840` build flag mirrors the define used by FastLED's
FastPin variant block (FastLED/FastLED#2445) so the variant code path is exercised
in CI.

Variant header: https://github.com/pdcook/nRFMicro-Arduino-Core/blob/main/variants/SuperMini_nRF52840/variant.h
