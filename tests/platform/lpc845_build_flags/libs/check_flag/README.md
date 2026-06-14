# `check_flag`

Probe library for the `lpc845_build_flags` regression fixture
(FastLED/fbuild#587). `#error`s out unless `-DFROM_PLATFORMIO_INI=1`
from `[env:*] build_flags` reaches the nxplpc library compile path.

See the fixture's top-level [`README.md`](../../README.md).
