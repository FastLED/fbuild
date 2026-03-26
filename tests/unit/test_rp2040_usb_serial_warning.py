"""
Unit tests for the USB serial default warning emitted by OrchestratorRP2040.

Tests verify that OrchestratorRP2040._check_usb_serial_default() emits a
log_warning for all RP2040 builds (because the arduino-pico framework uses
USB CDC for Serial by default), and that the warning is suppressed when the
user explicitly opts out with -DPICO_STDIO_USB=0 in build_flags.

The arduino-pico framework routes Serial.print() over USB CDC by default.
When no USB host is connected at power-on, Serial.print() may block for
several seconds before timing out — a foot gun for LED installations and
other deployed (non-USB) setups.
"""

from unittest.mock import patch

from fbuild.build.orchestrator_rp2040 import OrchestratorRP2040
from fbuild.packages import Cache


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _make_orch() -> OrchestratorRP2040:
    return OrchestratorRP2040(Cache(), verbose=False)


# ---------------------------------------------------------------------------
# Tests: method existence
# ---------------------------------------------------------------------------


def test_check_usb_serial_default_method_exists():
    """_check_usb_serial_default() must exist and be callable on OrchestratorRP2040."""
    orch = _make_orch()
    assert hasattr(orch, "_check_usb_serial_default")
    assert callable(orch._check_usb_serial_default)


# ---------------------------------------------------------------------------
# Tests: warning IS emitted
# ---------------------------------------------------------------------------


def test_warning_emitted_by_default_no_flags():
    """Warning is emitted for a plain RP2040 build with no user build_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", [])

    mock_warn.assert_called_once()


def test_warning_emitted_for_pico2():
    """Warning fires for Raspberry Pi Pico 2 (RP2350) as well."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico2", [])

    mock_warn.assert_called_once()


def test_warning_message_contains_board_id():
    """Warning message must identify the board."""
    board_id = "rpipico"
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default(board_id, [])

    warning_text = mock_warn.call_args[0][0]
    assert board_id in warning_text


def test_warning_message_mentions_usb_cdc():
    """Warning message must mention USB CDC."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", [])

    warning_text = mock_warn.call_args[0][0]
    assert "USB" in warning_text


def test_warning_message_mentions_blocking():
    """Warning message must mention the potential blocking behaviour."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", [])

    warning_text = mock_warn.call_args[0][0]
    assert "block" in warning_text.lower()


def test_warning_emitted_with_unrelated_flags():
    """Warning still fires when build_flags contains unrelated flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", ["-DFASTLED_ESP32_I2S", "-Os"])

    mock_warn.assert_called_once()


def test_warning_emitted_exactly_once():
    """_check_usb_serial_default() must emit at most one warning per call."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", [])

    assert mock_warn.call_count == 1


# ---------------------------------------------------------------------------
# Tests: warning is NOT emitted
# ---------------------------------------------------------------------------


def test_no_warning_when_pico_stdio_usb_disabled():
    """No warning when user explicitly opts out of USB stdio with -DPICO_STDIO_USB=0."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default("rpipico", ["-DPICO_STDIO_USB=0"])

    mock_warn.assert_not_called()


def test_no_warning_when_opt_out_mixed_with_other_flags():
    """No warning when -DPICO_STDIO_USB=0 appears alongside other build_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_rp2040.log_warning") as mock_warn:
        orch._check_usb_serial_default(
            "rpipico",
            ["-DFASTLED_ESP32_I2S", "-DPICO_STDIO_USB=0", "-Os"],
        )

    mock_warn.assert_not_called()
