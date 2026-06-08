// SPDX-License-Identifier: MIT
//
// NXP LPC8xx — hand-rolled Arduino `main()` shim (Stage 2 of #487).
//
// The Stage-1 LPC8xx test fixtures (tests/platform/lpc845/lpc845.ino,
// tests/platform/lpc804/lpc804.ino) define `setup()` and `loop()` but
// have no `main()`. The orchestrator embeds this file and emits it into
// the build dir as a third source so the linker has an entry point.
//
// This shim is intentionally minimal — it does NOT touch peripherals,
// does NOT initialise GPIO, does NOT set up timing. Stage-1 startup_.S
// runs SystemInit() (clock + flash wait states) before this `main()` is
// reached. Anything else the sketch needs must be done in `setup()`.
//
// This shim is replaced by the framework-owned `main()` in
// `zackees/ArduinoCore-LPC8xx::cores/lpc8xx/main.cpp` once #479 lands
// (tracked in #487, Stage 4: "vendor the framework into fbuild").

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
