// LPC845 build_flags propagation regression fixture (FastLED/fbuild#587).
//
// Pairs with `libs/check_flag/src/check_flag.cpp`, which `#error`s out unless
// `-DFROM_PLATFORMIO_INI=1` from `platformio.ini`'s `build_flags` reaches the
// nxplpc library compile path. A working build proves the orchestrator now
// folds `ctx.user_flags` into the `LibraryBuildEnv` flag set.

#include <Arduino.h>

extern "C" void check_flag_no_op(void);

void setup() {
    check_flag_no_op();
}

void loop() {
}
