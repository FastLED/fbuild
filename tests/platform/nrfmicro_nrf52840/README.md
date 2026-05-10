# nRFMicro nRF52840 Test Project

Minimal blink sketch for the nRFMicro community board build validation.

The board reuses the `nrf52840_dk_adafruit` PlatformIO board because no first-party
PlatformIO board package exists for this community variant. The `-DTARGET_NRFMICRO`
build flag mirrors the define used by FastLED's FastPin variant block
(FastLED/FastLED#2445) so the variant code path is exercised in CI.

Variant header: https://github.com/pdcook/nRFMicro-Arduino-Core/blob/main/variants/nRFMicro/variant.h
