# Daemon examples

`prebuilt_first_compile` measures source versus prebuilt cold/warm operation using
an ESP32 cross-compiler and a small local framework fixture. It uses real object
compilation, real local HTTP, daemon extraction, and byte-identical firmware and
ELF-section checks. It is not the Blink/FastLED production acceptance gate.
