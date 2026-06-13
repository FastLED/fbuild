// SPDX-License-Identifier: MIT
//
// NXP LPC8xx — bundled Arduino `main()` (#487 Stage 3, #479).
//
// The LPC8xx test fixtures (tests/platform/lpc804/lpc804.ino,
// tests/platform/lpc845/lpc845.ino) define `setup()` and `loop()` but
// have no `main()`. The orchestrator embeds this file alongside the rest
// of the bundled `arduino_stub` core (Arduino.h, wiring_digital.c,
// wiring_time.c, …) and emits it into the build dir so the linker has an
// entry point. It mirrors `zackees/ArduinoCore-LPC8xx`'s framework-owned
// `main()`; a project that ships that variant core simply overrides it.
//
// This `main()` is intentionally minimal — it does NOT touch peripherals,
// does NOT initialise GPIO, does NOT set up timing. startup_.S runs
// SystemInit() (clock + flash wait states) before this `main()` is
// reached. Anything else the sketch needs must be done in `setup()`.

// User-provided Arduino entry points. The .ino preprocessor emits normal
// C++ prototypes, so keep these declarations in C++ linkage.
void setup(void);
void loop(void);

int main(void) {
    setup();
    for (;;) {
        loop();
    }
}
