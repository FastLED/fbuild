# ClearCore Platform Test

Compile-only smoke test for the Teknic ClearCore board and its ATSAME53N19A
microcontroller. The fbuild package integration pins Teknic's official
ClearCore Arduino core 1.7.4, including the vendor linker script and
precompiled ClearCore/LwIP support libraries.

This test proves that a firmware image links for the real ClearCore target. It
does not claim hardware timing or electrical validation.

Run from this directory with:

```bash
fbuild build --env clearcore
```
