# Adafruit QT Py M0 Test Fixture

ARM Cortex-M0+ build validation project for the Adafruit QT Py M0 (SAMD21E18A, the smaller `E` variant of the SAMD21 family). Uses the Arduino framework on the `atmelsam` platform.

Tracks FastLED issues #1354 and #1381 — both report runtime/pin-map problems on the QT Py M0. fbuild's role here is to prove the board's toolchain + Arduino core compile cleanly; FastLED's `platforms/arm/d21` headers handle the pin map.
