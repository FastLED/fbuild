"""
Unit tests for the USB Serial mode warning emitted by OrchestratorTeensy.

Tests verify that OrchestratorTeensy._check_usb_serial_mode() emits a
log_warning when USB_SERIAL is the effective USB mode (meaning Serial.print()
communicates over USB), and that the warning is suppressed when the user
overrides the USB mode to a different type via build_flags.

All Teensy boards default to USB_SERIAL mode (USB CDC for Serial).  When no
USB host is connected, each Serial.print() call may stall for up to ~120 ms
before timing out.  In timing-sensitive sketches this is a subtle foot gun.
"""

from unittest.mock import patch

from fbuild.build.orchestrator_teensy import OrchestratorTeensy
from fbuild.packages import Cache


# ---------------------------------------------------------------------------
# Minimal platform_config stubs
# ---------------------------------------------------------------------------


class _FakePlatformConfig:
    """Minimal stand-in for a BoardConfigModel that carries a 'defines' list."""

    def __init__(self, defines):
        self.defines = defines


# Platform config that sets USB_SERIAL (the Teensy default)
PLATFORM_CONFIG_USB_SERIAL = _FakePlatformConfig(
    defines=[
        "__IMXRT1062__",
        "ARDUINO_ARCH_TEENSY",
        "ARDUINO_TEENSY41",
        ["ARDUINO", "10819"],
        ["TEENSYDUINO", "159"],
        "USB_SERIAL",
        "LAYOUT_US_ENGLISH",
    ]
)

# Platform config with no USB-mode define at all
PLATFORM_CONFIG_NO_USB_DEFINE = _FakePlatformConfig(
    defines=[
        "__IMXRT1062__",
        "ARDUINO_ARCH_TEENSY",
        "ARDUINO_TEENSY41",
    ]
)


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _make_orch() -> OrchestratorTeensy:
    return OrchestratorTeensy(Cache(), verbose=False)


# ---------------------------------------------------------------------------
# Tests: method existence
# ---------------------------------------------------------------------------


def test_check_usb_serial_mode_method_exists():
    """_check_usb_serial_mode() must exist and be callable on OrchestratorTeensy."""
    orch = _make_orch()
    assert hasattr(orch, "_check_usb_serial_mode")
    assert callable(orch._check_usb_serial_mode)


# ---------------------------------------------------------------------------
# Tests: warning IS emitted
# ---------------------------------------------------------------------------


def test_warning_emitted_when_usb_serial_in_platform_config():
    """Warning fires when platform config contains USB_SERIAL define."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], PLATFORM_CONFIG_USB_SERIAL)

    mock_warn.assert_called_once()


def test_warning_message_contains_board_id():
    """Warning message must identify the board."""
    board_id = "teensy41"
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode(board_id, [], PLATFORM_CONFIG_USB_SERIAL)

    warning_text = mock_warn.call_args[0][0]
    assert board_id in warning_text


def test_warning_message_mentions_usb_serial():
    """Warning message must mention USB_SERIAL."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], PLATFORM_CONFIG_USB_SERIAL)

    warning_text = mock_warn.call_args[0][0]
    assert "USB_SERIAL" in warning_text


def test_warning_message_mentions_stall():
    """Warning message must mention the potential stall/delay."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], PLATFORM_CONFIG_USB_SERIAL)

    warning_text = mock_warn.call_args[0][0]
    assert "stall" in warning_text.lower() or "block" in warning_text.lower() or "120" in warning_text


def test_warning_emitted_when_user_re_enables_usb_serial_via_flags():
    """Warning fires when the user explicitly adds -DUSB_SERIAL to build_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", ["-DUSB_SERIAL"], PLATFORM_CONFIG_NO_USB_DEFINE)

    mock_warn.assert_called_once()


def test_warning_emitted_exactly_once():
    """_check_usb_serial_mode() must emit at most one warning per call."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], PLATFORM_CONFIG_USB_SERIAL)

    assert mock_warn.call_count == 1


# ---------------------------------------------------------------------------
# Tests: warning is NOT emitted
# ---------------------------------------------------------------------------


def test_no_warning_when_no_usb_define():
    """No warning when platform config has no USB_* define at all."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], PLATFORM_CONFIG_NO_USB_DEFINE)

    mock_warn.assert_not_called()


def test_no_warning_when_platform_config_is_none():
    """No warning when platform_config is None (graceful handling)."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode("teensy41", [], None)

    mock_warn.assert_not_called()


def test_no_warning_when_user_overrides_to_usb_serial_hid():
    """No warning when user overrides USB mode to USB_SERIAL_HID via build_flags.

    The user's build_flags are applied after the platform-config defines, so
    the last USB_* definition wins (C preprocessor semantics).
    """
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode(
            "teensy41",
            ["-DUSB_SERIAL_HID"],  # changes effective mode away from USB_SERIAL
            PLATFORM_CONFIG_USB_SERIAL,
        )

    mock_warn.assert_not_called()


def test_no_warning_when_user_overrides_to_usb_midi():
    """No warning when user overrides USB mode to USB_MIDI via build_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode(
            "teensy41",
            ["-DUSB_MIDI"],
            PLATFORM_CONFIG_USB_SERIAL,
        )

    mock_warn.assert_not_called()


def test_last_definition_wins_user_enables_usb_serial_after_hid():
    """If platform has no USB_SERIAL but user adds then re-adds, last wins.

    If the user adds -DUSB_MIDI first and then -DUSB_SERIAL last, the effective
    mode is USB_SERIAL and the warning is emitted.
    """
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_teensy.log_warning") as mock_warn:
        orch._check_usb_serial_mode(
            "teensy41",
            ["-DUSB_MIDI", "-DUSB_SERIAL"],  # last flag wins
            PLATFORM_CONFIG_NO_USB_DEFINE,
        )

    mock_warn.assert_called_once()
