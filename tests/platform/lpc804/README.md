# LPC804 build fixture

Bare-metal CMSIS test project for the NXP LPC804 (Cortex-M0+, 32 KB Flash, 4 KB RAM).

Stage 1 of FastLED/FastLED#2836. The CI workflow that drives this fixture
(`.github/workflows/build-lpc804.yml`) will FAIL until the Stage 2 FastLED-side
port lands and the LPC8xx orchestrator can compile a real binary.
