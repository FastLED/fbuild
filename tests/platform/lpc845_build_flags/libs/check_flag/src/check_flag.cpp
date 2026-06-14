// FastLED/fbuild#587 regression probe.
//
// The library is reached via `lib_extra_dirs = libs` in the fixture's
// `platformio.ini`. The nxplpc orchestrator must fold `[env:*] build_flags`
// (i.e. `ctx.user_flags`) into the `LibraryBuildEnv` flag set before it
// compiles extra-library sources. If that fold is missing, this `#error`
// fires and the build fails — which is exactly the gap that PR #576 worked
// around at the board-JSON level (`lpc845brk.json`'s `-DRELEASE=1
// -DFASTLED_DISABLE_DBG=1`) and that #587 retires.

#ifndef FROM_PLATFORMIO_INI
#error "FastLED/fbuild#587 regression: [env:lpc845brk] build_flags did not reach the nxplpc library compile path"
#endif

extern "C" void check_flag_no_op(void) {}
