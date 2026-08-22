# fbuild-toolchain integration tests

Tests that exercise toolchain resolution against the real network and the real
package cache, rather than in-process fixtures.

- **`qemu_linux_runtime.rs`** — Linux-only. Proves fbuild can start Espressif
  QEMU on a host that carries none of libslirp / libSDL2 / libpixman, by
  provisioning its own runtime-library bundle. `#[ignore]`d by default because
  it downloads QEMU (~15 MB) and, when needed, the bundle (~5 MB). Run in CI by
  `.github/workflows/qemu-linux-runtime.yml` on a stock `ubuntu-latest` runner
  with **no** apt preinstall step — that missing preinstall is the point of the
  test.

Run locally on Linux:

```bash
soldr cargo test -p fbuild-toolchain --test qemu_linux_runtime -- --ignored --nocapture
```
