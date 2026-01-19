"""
Device Discovery - Enumerate and identify connected serial devices.

This module provides device enumeration using pyserial, creating stable device
identifiers for physical boards. It supports:

- USB serial devices with unique serial numbers (preferred identifier)
- VID/PID-based identification for devices without serial numbers
- QEMU virtual devices (placeholder support)
- Human-readable device descriptions

The stable device ID is crucial for multi-board concurrent development,
ensuring that device leases remain valid across reconnections.
"""

import _thread
import hashlib
import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

# Import pyserial types for type checking only
if TYPE_CHECKING:
    from serial.tools.list_ports_common import ListPortInfo

try:
    import serial.tools.list_ports as list_ports

    HAS_SERIAL = True
except ImportError:
    list_ports = None
    HAS_SERIAL = False


@dataclass
class DeviceInfo:
    """Information about a discovered serial device.

    Attributes:
        port: The serial port path (e.g., "COM3" or "/dev/ttyUSB0")
        device_id: Stable unique identifier for this device
        vid: USB Vendor ID (None if not available)
        pid: USB Product ID (None if not available)
        serial_number: USB serial number (None if not available)
        description: Human-readable device description
        manufacturer: Device manufacturer (None if not available)
        product: Product name (None if not available)
        hwid: Hardware ID string from the system
        is_qemu: True if this is a QEMU virtual device
    """

    port: str
    device_id: str
    vid: int | None
    pid: int | None
    serial_number: str | None
    description: str
    manufacturer: str | None
    product: str | None
    hwid: str
    is_qemu: bool = False

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "port": self.port,
            "device_id": self.device_id,
            "vid": self.vid,
            "pid": self.pid,
            "serial_number": self.serial_number,
            "description": self.description,
            "manufacturer": self.manufacturer,
            "product": self.product,
            "hwid": self.hwid,
            "is_qemu": self.is_qemu,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeviceInfo":
        """Create DeviceInfo from dictionary."""
        return cls(
            port=data["port"],
            device_id=data["device_id"],
            vid=data.get("vid"),
            pid=data.get("pid"),
            serial_number=data.get("serial_number"),
            description=data.get("description", ""),
            manufacturer=data.get("manufacturer"),
            product=data.get("product"),
            hwid=data.get("hwid", ""),
            is_qemu=data.get("is_qemu", False),
        )

    def matches_port(self, port: str) -> bool:
        """Check if this device matches a given port name.

        Handles case-insensitive comparison for Windows COM ports.
        """
        return self.port.lower() == port.lower()


def _generate_device_id_from_serial(serial_number: str) -> str:
    """Generate a stable device ID from a USB serial number.

    The USB serial number is the most stable identifier for a device,
    as it remains constant regardless of which port the device is
    plugged into.

    Args:
        serial_number: The USB serial number

    Returns:
        A stable device ID string prefixed with "usb-"
    """
    # Clean up the serial number (remove any whitespace)
    clean_serial = serial_number.strip()
    return f"usb-{clean_serial}"


def _generate_device_id_from_hardware(
    vid: int | None,
    pid: int | None,
    port: str,
) -> str:
    """Generate a device ID from VID/PID and port path.

    This is a fallback for devices without USB serial numbers.
    The device ID includes a hash of the port path for uniqueness,
    but this makes it less stable across port changes.

    Args:
        vid: USB Vendor ID
        pid: USB Product ID
        port: The serial port path

    Returns:
        A device ID string prefixed with "hw-"
    """
    # Create a hash from the combination
    vid_str = f"{vid:04x}" if vid else "0000"
    pid_str = f"{pid:04x}" if pid else "0000"

    # Include port in hash for uniqueness when VID/PID aren't available
    combined = f"{vid_str}:{pid_str}:{port}"
    hash_suffix = hashlib.sha256(combined.encode()).hexdigest()[:8]

    return f"hw-{vid_str}-{pid_str}-{hash_suffix}"


def _generate_qemu_device_id(identifier: str) -> str:
    """Generate a device ID for a QEMU virtual device.

    Args:
        identifier: A unique identifier for the QEMU instance

    Returns:
        A device ID string prefixed with "qemu-"
    """
    return f"qemu-{identifier}"


def _port_info_to_device_info(port_info: "ListPortInfo") -> DeviceInfo:
    """Convert a pyserial ListPortInfo to our DeviceInfo.

    Args:
        port_info: Port information from pyserial

    Returns:
        DeviceInfo with stable device ID
    """
    # Extract basic information
    port = port_info.device
    vid = port_info.vid
    pid = port_info.pid
    serial_number = port_info.serial_number
    description = port_info.description or ""
    manufacturer = port_info.manufacturer
    product = port_info.product
    hwid = port_info.hwid or ""

    # Generate stable device ID
    # Prefer USB serial number if available
    if serial_number and serial_number.strip():
        device_id = _generate_device_id_from_serial(serial_number)
    else:
        device_id = _generate_device_id_from_hardware(vid, pid, port)

    return DeviceInfo(
        port=port,
        device_id=device_id,
        vid=vid,
        pid=pid,
        serial_number=serial_number,
        description=description,
        manufacturer=manufacturer,
        product=product,
        hwid=hwid,
        is_qemu=False,
    )


def discover_devices(
    include_links: bool = False,
) -> list[DeviceInfo]:
    """Enumerate all connected serial devices.

    This function discovers all serial ports on the system and creates
    DeviceInfo objects with stable device IDs for each.

    Args:
        include_links: Whether to include symbolic links (Unix only).
            On Windows this has no effect.

    Returns:
        List of DeviceInfo objects for all discovered devices,
        sorted by port name.

    Raises:
        RuntimeError: If pyserial is not installed

    Example:
        >>> devices = discover_devices()
        >>> for device in devices:
        ...     print(f"{device.port}: {device.device_id} - {device.description}")
        COM3: usb-A50285BI - Silicon Labs CP210x USB to UART Bridge
        COM4: hw-10c4-ea60-abc12345 - USB Serial Device
    """
    if not HAS_SERIAL or list_ports is None:
        raise RuntimeError("pyserial is required for device discovery. Install it with: pip install pyserial")

    devices: list[DeviceInfo] = []

    try:
        ports = list_ports.comports(include_links=include_links)

        for port_info in ports:
            try:
                device_info = _port_info_to_device_info(port_info)
                devices.append(device_info)
                logging.debug(f"Discovered device: {device_info.port} (id={device_info.device_id}, desc={device_info.description})")
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to process port {port_info.device}: {e}")

    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error enumerating serial ports: {e}")
        raise

    # Sort by port name for consistent ordering
    devices.sort(key=lambda d: d.port.lower())

    logging.info(f"Discovered {len(devices)} serial device(s)")
    return devices


def get_device_by_port(port: str) -> DeviceInfo | None:
    """Get device information for a specific port.

    This is a convenience function that discovers all devices and
    returns the one matching the specified port.

    Args:
        port: The serial port to look up (e.g., "COM3" or "/dev/ttyUSB0")

    Returns:
        DeviceInfo for the port, or None if not found
    """
    try:
        devices = discover_devices()
        for device in devices:
            if device.matches_port(port):
                return device
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error looking up device for port {port}: {e}")

    return None


def get_device_id(port: str) -> str:
    """Get stable device ID for a port.

    This function looks up the device connected to a port and returns
    its stable device ID. If the device cannot be found, it generates
    a fallback ID based on the port name.

    Args:
        port: The serial port (e.g., "COM3" or "/dev/ttyUSB0")

    Returns:
        A stable device ID string
    """
    device = get_device_by_port(port)
    if device:
        return device.device_id

    # Fallback: generate ID from port name
    logging.warning(f"Could not find device for port {port}, using port-based ID")
    port_hash = hashlib.sha256(port.encode()).hexdigest()[:8]
    return f"port-{port_hash}"


def create_qemu_device(
    instance_id: str,
    description: str = "QEMU Virtual Device",
) -> DeviceInfo:
    """Create a DeviceInfo for a QEMU virtual device.

    QEMU devices don't have physical serial ports but still need
    device IDs for lease management.

    Args:
        instance_id: Unique identifier for the QEMU instance
        description: Human-readable description

    Returns:
        DeviceInfo for the QEMU device
    """
    device_id = _generate_qemu_device_id(instance_id)

    return DeviceInfo(
        port=f"qemu:{instance_id}",
        device_id=device_id,
        vid=None,
        pid=None,
        serial_number=None,
        description=description,
        manufacturer="QEMU",
        product="Virtual Device",
        hwid=f"QEMU\\{instance_id}",
        is_qemu=True,
    )


def find_device_by_id(device_id: str) -> DeviceInfo | None:
    """Find a device by its stable device ID.

    This function discovers all devices and returns the one with
    the matching device ID.

    Args:
        device_id: The stable device ID to search for

    Returns:
        DeviceInfo if found, None otherwise
    """
    try:
        devices = discover_devices()
        for device in devices:
            if device.device_id == device_id:
                return device
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error searching for device {device_id}: {e}")

    return None


def find_devices_by_vid_pid(
    vid: int,
    pid: int,
) -> list[DeviceInfo]:
    """Find all devices matching a VID/PID combination.

    Useful for finding all devices of a specific type (e.g., all
    ESP32-C6 development boards).

    Args:
        vid: USB Vendor ID to match
        pid: USB Product ID to match

    Returns:
        List of matching DeviceInfo objects
    """
    try:
        devices = discover_devices()
        return [d for d in devices if d.vid == vid and d.pid == pid]
    except KeyboardInterrupt:
        _thread.interrupt_main()
        raise
    except Exception as e:
        logging.error(f"Error searching for devices with VID={vid:04x} PID={pid:04x}: {e}")
        return []


def is_esp32_device(device: DeviceInfo) -> bool:
    """Check if a device appears to be an ESP32 board.

    This uses heuristics based on common ESP32 USB-UART chips
    and description patterns.

    Args:
        device: The device to check

    Returns:
        True if the device appears to be an ESP32
    """
    # Common ESP32 USB-UART chip VID/PIDs
    esp32_vid_pids = [
        (0x10C4, 0xEA60),  # Silicon Labs CP210x
        (0x1A86, 0x7523),  # QinHeng CH340
        (0x1A86, 0x55D4),  # QinHeng CH9102
        (0x303A, 0x1001),  # Espressif ESP32-S2
        (0x303A, 0x1002),  # Espressif ESP32-S3
        (0x303A, 0x0002),  # Espressif ESP32-S2 (CDC)
        (0x303A, 0x1002),  # Espressif ESP32-S3 (CDC)
        (0x303A, 0x4001),  # Espressif ESP32-C3/C6 JTAG
        (0x0403, 0x6001),  # FTDI FT232
        (0x0403, 0x6010),  # FTDI FT2232
        (0x0403, 0x6011),  # FTDI FT4232
        (0x0403, 0x6014),  # FTDI FT232H
        (0x0403, 0x6015),  # FTDI FT-X
    ]

    if device.vid and device.pid:
        if (device.vid, device.pid) in esp32_vid_pids:
            return True

    # Check description for ESP32 keywords
    desc_lower = device.description.lower()
    esp_keywords = ["esp32", "esp-32", "espressif", "cp210x", "ch340", "ch9102"]
    return any(kw in desc_lower for kw in esp_keywords)


def get_device_summary(device: DeviceInfo) -> str:
    """Get a human-readable summary of a device.

    Args:
        device: The device to summarize

    Returns:
        A formatted string summary
    """
    parts = [device.port]

    if device.description:
        parts.append(f"- {device.description}")

    if device.manufacturer:
        parts.append(f"({device.manufacturer})")

    if device.is_qemu:
        parts.append("[QEMU]")

    return " ".join(parts)


# Well-known VID/PID constants for common development boards
class KnownDevices:
    """Well-known VID/PID combinations for common devices."""

    # Silicon Labs CP210x (common on ESP32 boards)
    CP210X_VID = 0x10C4
    CP210X_PID = 0xEA60

    # QinHeng CH340 (common on cheap ESP32 boards)
    CH340_VID = 0x1A86
    CH340_PID = 0x7523

    # QinHeng CH9102 (newer version)
    CH9102_VID = 0x1A86
    CH9102_PID = 0x55D4

    # Espressif native USB (ESP32-S2/S3/C3/C6)
    ESPRESSIF_VID = 0x303A

    # FTDI (common on many dev boards)
    FTDI_VID = 0x0403
