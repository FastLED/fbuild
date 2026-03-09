"""
Firmware deployment module for uploading to Teensy devices.

This module handles flashing firmware to Teensy boards using teensy_loader_cli.

Reboot strategy (to enter bootloader mode without pressing PROGRAM button):

The Teensy 4.x has a two-chip architecture: the main NXP i.MX RT1062 processor
and a separate MKL02Z32 bootloader chip connected via JTAG. When firmware calls
_reboot_Teensyduino_(), it executes `bkpt #251` which halts the Cortex-M7.
The MKL02 detects this via JTAG and presents itself as a USB HID device
(the HalfKay bootloader, VID 0x16C0, PID 0x0478).

Two USB-based reboot methods exist, depending on the firmware's USB type:

1. CDC Serial (PID 0x0483, or composite types with Serial like 0x0489):
   Open the COM port at 134 baud. The firmware detects the SET_LINE_CODING
   request and starts an 80ms SOF countdown to _reboot_Teensyduino_().

2. Non-Serial USB types (MIDI-only, RawHID, Keyboard, etc.):
   Send HID Feature Report with magic bytes [0xA9, 0x45, 0xC2, 0x6B] to
   the SEREMU (Serial Emulation) interface. Same 80ms countdown.

Note: `teensy_loader_cli -s` only works on Linux (via libusb); on Windows it
silently does nothing. That's why we handle the reboot ourselves.
"""

import logging
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

from fbuild.deploy.deployer import DeploymentError, DeploymentResult, IDeployer
from fbuild.deploy.platform_utils import get_filtered_env
from fbuild.subprocess_utils import safe_run

# PJRC USB Vendor ID
_PJRC_VID = 0x16C0

# Teensy USB Product IDs
_PID_HALFKAY = 0x0478  # HalfKay bootloader
_PID_SERIAL = 0x0483  # Teensyduino Serial
_PID_REBOOTOR = 0x0477  # External rebootor device

# PIDs that include CDC Serial (134-baud trick works)
_SERIAL_PIDS = {
    0x0483,  # Serial
    0x0489,  # Serial + MIDI
    0x048A,  # Serial + MTP
    0x0482,  # Serial + Keyboard + Mouse + Joystick
    0x0487,  # Serial + MIDI + Audio
    0x048B,  # Serial + Audio
    0x048C,  # Serial + MTP + Disk
}

# PIDs that use SEREMU (HID magic bytes work)
_SEREMU_PIDS = {
    0x0485,  # MIDI (no Serial)
    0x0486,  # RawHID
    0x0488,  # Flight Sim
    0x0481,  # Keyboard
    0x0480,  # Joystick
    0x0484,  # Disk (Experimental)
}

# HID magic bytes to trigger _reboot_Teensyduino_() via SEREMU interface
_REBOOT_MAGIC = bytes([0xA9, 0x45, 0xC2, 0x6B])

# Map environment names to MCU identifiers for teensy_loader_cli
_ENV_TO_MCU: dict[str, str] = {
    "teensy41": "TEENSY41",
    "teensy40": "TEENSY40",
    "teensylc": "TEENSYLC",
    "teensy36": "TEENSY36",
    "teensy35": "TEENSY35",
    "teensy31": "TEENSY31",
}

logger = logging.getLogger(__name__)


def _find_tool(name: str) -> Path | None:
    """Find a tool executable in PlatformIO's tool-teensy package."""
    from fbuild.paths import get_platformio_package

    base = get_platformio_package("tool-teensy")
    for suffix in (".exe", ""):
        candidate = base / f"{name}{suffix}"
        if candidate.exists():
            return candidate
    return None


def _get_mcu_for_env(env_name: str) -> str | None:
    """Map environment name to MCU identifier for teensy_loader_cli."""
    return _ENV_TO_MCU.get(env_name.lower())


def _find_teensy_com_port() -> str | None:
    """Auto-detect the Teensy's COM port by scanning for PJRC VID."""
    try:
        import serial.tools.list_ports

        for port_info in serial.tools.list_ports.comports():
            if port_info.vid == _PJRC_VID and port_info.pid in _SERIAL_PIDS:
                return port_info.device
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise
    except Exception:
        pass
    return None


def _is_teensy_in_bootloader() -> bool:
    """Check if a Teensy is already in HalfKay bootloader mode (HID device)."""
    try:
        import hid

        devices = hid.enumerate(_PJRC_VID, _PID_HALFKAY)
        return len(devices) > 0
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise
    except Exception:
        pass
    return False


def _trigger_reboot_134_baud(port: str | None, verbose: bool) -> bool:
    """Trigger Teensy reboot via 134-baud CDC SET_LINE_CODING.

    Works for any USB type that includes CDC Serial (PID 0x0483, 0x0489, etc.).
    """
    if not port:
        # Auto-detect Teensy COM port
        port = _find_teensy_com_port()
        if not port:
            return False

    try:
        import serial

        if verbose:
            print(f"Triggering Teensy reboot via 134-baud on {port}...")

        ser = serial.Serial()
        ser.port = port
        ser.baudrate = 134
        ser.timeout = 1
        ser.open()
        time.sleep(0.1)
        ser.close()

        if verbose:
            print("134-baud reboot signal sent, waiting for bootloader...")

        # Wait for the Teensy to reboot (~80ms SOF timer + USB re-enumeration)
        time.sleep(2.0)
        return True

    except ImportError:
        logger.debug("pyserial not available for 134-baud reboot")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise
    except Exception as e:
        logger.debug(f"134-baud reboot failed on {port}: {e}")
        if verbose:
            print(f"134-baud reboot failed: {e}")
        return False


def _trigger_reboot_hid_magic(verbose: bool) -> bool:
    """Trigger Teensy reboot via HID magic bytes to SEREMU interface.

    Works for non-Serial USB types (MIDI, RawHID, Keyboard, etc.) that have
    a SEREMU (Serial Emulation) HID interface.
    """
    try:
        import hid

        # Search for any Teensy device with SEREMU interface
        for pid in _SEREMU_PIDS | _SERIAL_PIDS:
            devices = hid.enumerate(_PJRC_VID, pid)
            for dev_info in devices:
                try:
                    h = hid.device()
                    h.open_path(dev_info["path"])
                    if verbose:
                        print(f"Sending HID reboot magic to VID:PID={_PJRC_VID:#06x}:{pid:#06x} interface={dev_info.get('interface_number', '?')}...")
                    # Send Feature Report: Report ID 0 + magic bytes
                    h.send_feature_report(b"\x00" + _REBOOT_MAGIC)
                    h.close()
                    if verbose:
                        print("HID reboot magic sent, waiting for bootloader...")
                    time.sleep(2.0)
                    return True
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

                    handle_keyboard_interrupt_properly(ke)
                    raise
                except Exception as e:
                    logger.debug(f"HID magic failed for {dev_info['path']}: {e}")
                    continue

    except ImportError:
        logger.debug("hidapi not available for HID magic reboot")
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise
    except Exception as e:
        logger.debug(f"HID magic reboot failed: {e}")

    return False


def _trigger_reboot_teensy_reboot(verbose: bool) -> bool:
    """Trigger Teensy reboot using PlatformIO's teensy_reboot tool.

    teensy_reboot is a closed-source PJRC binary that handles both
    Serial (134-baud) and HID (magic bytes) USB types.
    """
    rebooter = _find_tool("teensy_reboot")
    if not rebooter:
        return False

    try:
        if verbose:
            print(f"Triggering Teensy reboot via {rebooter.name}...")

        env = get_filtered_env() if sys.platform == "win32" else None
        result = safe_run(
            [str(rebooter)],
            capture_output=True,
            timeout=10,
            env=env,
        )

        if verbose:
            stdout = result.stdout.decode("utf-8", errors="replace") if result.stdout else ""
            stderr = result.stderr.decode("utf-8", errors="replace") if result.stderr else ""
            if stdout.strip():
                print(stdout.strip())
            if stderr.strip():
                print(stderr.strip())

        time.sleep(2.0)
        return result.returncode == 0

    except subprocess.TimeoutExpired:
        logger.debug("teensy_reboot timed out")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise
    except Exception as e:
        logger.debug(f"teensy_reboot failed: {e}")
        return False


def _trigger_reboot(port: str | None, verbose: bool) -> bool:
    """Try all available methods to reboot Teensy into bootloader mode.

    Tries in order:
    1. Check if already in bootloader mode (nothing to do)
    2. 134-baud serial trick (for CDC Serial USB types)
    3. HID magic bytes (for SEREMU/non-Serial USB types)
    4. teensy_reboot tool (fallback, handles both)

    Returns True if reboot was triggered (or device was already in bootloader).
    """
    # Already in bootloader?
    if _is_teensy_in_bootloader():
        if verbose:
            print("Teensy is already in bootloader mode")
        return True

    # Method 1: 134-baud serial trick
    if _trigger_reboot_134_baud(port, verbose):
        return True

    # Method 2: HID magic bytes via SEREMU
    if _trigger_reboot_hid_magic(verbose):
        return True

    # Method 3: teensy_reboot tool
    if _trigger_reboot_teensy_reboot(verbose):
        return True

    return False


class TeensyDeployer(IDeployer):
    """Handles firmware deployment to Teensy devices.

    Upload strategy:
    1. Trigger bootloader reboot via 134-baud, HID magic, or teensy_reboot
    2. Upload firmware via teensy_loader_cli -w (wait for bootloader device)
    """

    def __init__(self, verbose: bool = False):
        self.verbose = verbose

    def deploy(
        self,
        project_dir: Path,
        env_name: str,
        port: Optional[str] = None,
    ) -> DeploymentResult:
        try:
            loader = _find_tool("teensy_loader_cli")
            if not loader:
                raise DeploymentError("teensy_loader_cli not found. Install the Teensy platform in PlatformIO: ~/.platformio/packages/tool-teensy/")

            mcu = _get_mcu_for_env(env_name)
            if not mcu:
                raise DeploymentError(f"Unknown Teensy environment: {env_name}. Known environments: {', '.join(sorted(_ENV_TO_MCU.keys()))}")

            # Find firmware.hex
            from fbuild.paths import find_firmware, get_project_build_root

            hex_path = find_firmware(project_dir, env_name, "firmware.hex")

            if hex_path is None:
                raise DeploymentError(f"firmware.hex not found in {get_project_build_root(project_dir) / env_name}. Run 'fbuild build' first.")

            if self.verbose:
                print(f"Teensy MCU: {mcu}")
                print(f"Firmware: {hex_path}")
                print(f"Loader: {loader}")

            # Step 1: Trigger reboot into bootloader
            rebooted = _trigger_reboot(port, self.verbose)
            if not rebooted and self.verbose:
                print("Could not trigger soft reboot; teensy_loader_cli will wait for bootloader...")

            # Step 2: Upload firmware (with -w to wait for bootloader)
            return self._flash_teensy(loader, mcu, hex_path)

        except DeploymentError as e:
            return DeploymentResult(success=False, message=str(e))
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise
        except Exception as e:
            return DeploymentResult(success=False, message=f"Unexpected deployment error: {e}")

    def _flash_teensy(
        self,
        loader: Path,
        mcu: str,
        hex_path: Path,
    ) -> DeploymentResult:
        cmd = [
            str(loader),
            f"--mcu={mcu}",
            "-w",
            "-v",
            str(hex_path),
        ]

        if self.verbose:
            print(f"Running: {' '.join(cmd)}")

        try:
            env = get_filtered_env() if sys.platform == "win32" else None

            result = safe_run(
                cmd,
                capture_output=True,
                timeout=30,
                env=env,
            )

            stdout_text = result.stdout.decode("utf-8", errors="replace") if result.stdout else ""
            stderr_text = result.stderr.decode("utf-8", errors="replace") if result.stderr else ""

            if self.verbose:
                if stdout_text:
                    print(stdout_text)
                if stderr_text:
                    print(stderr_text)

            if result.returncode == 0:
                time.sleep(1.0)
                return DeploymentResult(
                    success=True,
                    message="Firmware uploaded successfully to Teensy",
                )

            error_detail = stderr_text or stdout_text or "Unknown error"
            return DeploymentResult(
                success=False,
                message=f"Teensy upload failed (exit code {result.returncode}): {error_detail}",
            )

        except subprocess.TimeoutExpired:
            return DeploymentResult(
                success=False,
                message=("Teensy upload timed out after 30s. The device may not be in bootloader mode. Try pressing the PROGRAM button manually, or check USB connection."),
            )
        except OSError as e:
            raise DeploymentError(f"Failed to run teensy_loader_cli: {e}") from e
