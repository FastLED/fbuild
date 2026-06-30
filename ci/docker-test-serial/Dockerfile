# Docker image for testing fbuild's serial-detection code against a
# real USB device that's been attached into WSL2 via usbipd-win
# (Windows host) or natively (Linux host).
#
# FastLED/fbuild#899 — replaces the manual setup described in the
# post-mortem with a reproducible Dockerfile. See
# `ci/docker-test-serial/README.md` for the host-side setup
# (one-time `usbipd-win` install + per-session `usbipd attach`).
#
# Build:
#   docker build -f ci/docker-test-serial/Dockerfile -t fbuild-test-serial .
#
# Run (with USB device passed through):
#   docker run --rm \
#     --device=/dev/ttyACM0 \
#     -v "$(pwd):/work" -w /work \
#     fbuild-test-serial \
#     bash ci/docker-test-serial/test.sh /dev/ttyACM0

FROM debian:12-slim

ENV DEBIAN_FRONTEND=noninteractive

# - usbutils: `lsusb` for diagnostic enumeration
# - udev:     populates /dev/ttyACM* nodes when the kernel sees a new
#             USB CDC device. Without this the device shows up in
#             /sys but not /dev.
# - ca-certificates: TLS for any HTTPS we end up making (cargo update,
#             toolchain download via soldr, etc.)
# - libudev1: runtime lib that serialport-rs's libudev-feature build
#             would need. Pulled in even when serialport is built
#             with default-features=false so the image stays
#             forward-compatible.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates \
        usbutils \
        udev \
        libudev1 \
 && rm -rf /var/lib/apt/lists/*

# Mount point for the cargo-built fbuild binary; we don't compile in
# the image because the source tree is bind-mounted at runtime via -v.
# Keeping the image lean (no Rust toolchain inside) cuts build time
# from minutes to ~10s and keeps the image under 100 MB.
WORKDIR /work

CMD ["bash"]
