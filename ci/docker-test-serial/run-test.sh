#!/usr/bin/env bash
# Real-device validation of FastLED/fbuild#895 + #897 CDC detection.
# Run from a normal Windows PowerShell or WSL bash after one-time setup
# (see setup-wsl-usb.ps1).
#
# Drives:
#   1. Confirm an Espressif device is bound + identifies its BUSID.
#   2. Attach it to Ubuntu WSL2 (user-mode, no UAC).
#   3. Build + run the in-WSL test that exercises:
#      - port_class::detect_port_kernel_class (sysfs read)
#      - boards::family_for_vid_pid (VID/PID table)
#      - boards::family_for_port (full chain, #897 disagreement warning)
#   4. Assert both signals say Esp32NativeUsbCdc / CdcAcm.
#   5. Detach.
#
# Expected output ends with:
#   *** REAL-DEVICE TEST: PASS ***
#
# Validated end-to-end at PR #898 closure against an ESP32-S3
# (VID 303A, PID 1001) on a Windows 10 host.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WSL_DISTRO="${WSL_DISTRO:-Ubuntu}"
PORT_IN_WSL="${PORT_IN_WSL:-/dev/ttyACM0}"

usbipd=$(cygpath -u 'C:\Program Files\usbipd-win\usbipd.exe' 2>/dev/null || echo '/c/Program Files/usbipd-win/usbipd.exe')

# 1. Find a 303A:1001 device
echo "==> [1/5] Looking for Espressif (303A:1001) device..."
busid=$("$usbipd" list | awk '/303a:1001.*Shared/ { print $1; exit }') || true
if [[ -z "$busid" ]]; then
    echo "FAIL: No 303A:1001 device bound/shared."
    echo "      Run setup-wsl-usb.ps1 elevated first."
    exit 1
fi
echo "    Using BUSID $busid"

# 2. Attach to Ubuntu
echo "==> [2/5] Attaching $busid to WSL distro '$WSL_DISTRO'..."
"$usbipd" attach --busid "$busid" --wsl="$WSL_DISTRO" 2>&1 | grep -v '^usbipd: info:' || true
sleep 3

# 3. Make sure cdc_acm is loaded (Ubuntu loads automatically, but
#    Alpine etc. needs explicit modprobe)
echo "==> [3/5] Ensure cdc_acm module loaded..."
wsl.exe -d "$WSL_DISTRO" --user root -- bash -c "lsmod | grep -q '^cdc_acm' || modprobe cdc_acm; lsmod | grep cdc_acm"

# 4. Build + run the in-WSL real-device test
echo "==> [4/5] Build + run the in-WSL test..."
wsl.exe -d "$WSL_DISTRO" --user root -- bash <<'WSL_SCRIPT'
set -euo pipefail
source /root/.cargo/env 2>/dev/null || (
    apt-get install -y -qq curl build-essential pkg-config 2>&1 | tail -1
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.94.1 --profile minimal -q 2>&1 | tail -1
    source /root/.cargo/env
)

mkdir -p /tmp/fbuild-test-run
cd /tmp/fbuild-test-run
cat > Cargo.toml <<'EOF'
[package]
name = "fbuild-real-device"
version = "0.0.1"
edition = "2021"

[dependencies]
fbuild-serial = { path = "/mnt/c/Users/niteris/dev/fbuild/crates/fbuild-serial" }
fbuild-core   = { path = "/mnt/c/Users/niteris/dev/fbuild/crates/fbuild-core" }

[[bin]]
name = "real"
path = "main.rs"
EOF
cat > main.rs <<'EOF'
use fbuild_serial::boards::{family_for_vid_pid, family_for_port, BoardFamily};
use fbuild_serial::port_class::{detect_port_kernel_class, PortKernelClass};
fn main() {
    let port = std::env::args().nth(1).unwrap_or_else(|| "/dev/ttyACM0".to_string());
    println!("=== fbuild real-device validation ({port}) ===\n");
    let kc = detect_port_kernel_class(&port);
    println!("port_class::detect_port_kernel_class -> {kc:?}");
    let vp = family_for_vid_pid(0x303A, 0x1001);
    println!("family_for_vid_pid(0x303A, 0x1001)   -> {vp:?}");
    let f = family_for_port(&port);
    println!("family_for_port(\"{port}\")           -> {f:?}");
    if let Some(ff) = f { println!("  idle_dtr_rts: {:?}\n  reset_method: {:?}", ff.idle_dtr_rts(), ff.reset_method()); }
    let kc_ok = matches!(kc, Some(PortKernelClass::CdcAcm));
    let f_ok = matches!(f, Some(BoardFamily::Esp32NativeUsbCdc));
    println!("\nkernel-class == CdcAcm:               {}", if kc_ok { "PASS" } else { "FAIL" });
    println!("family       == Esp32NativeUsbCdc:    {}", if f_ok { "PASS" } else { "FAIL" });
    if kc_ok && f_ok { println!("\n*** REAL-DEVICE TEST: PASS ***"); }
    else { println!("\n*** REAL-DEVICE TEST: FAIL ***"); std::process::exit(1); }
}
EOF
cargo build --release 2>&1 | tail -2
./target/release/real /dev/ttyACM0
WSL_SCRIPT

# 5. Detach (user-mode, leaves bind in place for next session)
echo "==> [5/5] Detach..."
"$usbipd" detach --busid "$busid" 2>&1 | grep -v '^usbipd: info:' || true

echo
echo "ALL DONE."
