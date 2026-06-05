# LPC845 build fixture

Bare-metal CMSIS test project for the NXP LPC845 (Cortex-M0+, 64 KB Flash, 16 KB RAM, DMA).

Stage 1 of FastLED/FastLED#2836. The CI workflow that drives this fixture
(`.github/workflows/build-lpc845.yml`) will FAIL until the Stage 2 FastLED-side
port lands and the LPC8xx orchestrator can compile a real binary.
