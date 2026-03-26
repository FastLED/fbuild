"""
Unit tests for the native USB CDC warning emitted by BuildOrchestratorAVR.

Tests verify that BuildOrchestratorAVR._check_native_usb_cdc() emits a
log_warning when the target MCU is ATmega32U4 (which has built-in USB CDC),
and that no warning is emitted for boards with a regular UART-only MCU.

ATmega32U4-based boards (Arduino Leonardo, Micro, Pro Micro, LilyPad USB,
etc.) route Serial.print() over USB CDC rather than a hardware UART.  When
no USB host is connected at power-on, Serial.print() blocks indefinitely —
the same foot gun as ARDUINO_USB_CDC_ON_BOOT=1 on ESP32.
"""

from unittest.mock import patch

from fbuild.build.orchestrator_avr import BuildOrchestratorAVR
from fbuild.packages import Cache


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _make_orch() -> BuildOrchestratorAVR:
    return BuildOrchestratorAVR(Cache(), verbose=False)


# ---------------------------------------------------------------------------
# Tests: method existence
# ---------------------------------------------------------------------------


def test_check_native_usb_cdc_method_exists():
    """_check_native_usb_cdc() must exist and be callable on BuildOrchestratorAVR."""
    orch = _make_orch()
    assert hasattr(orch, "_check_native_usb_cdc")
    assert callable(orch._check_native_usb_cdc)


# ---------------------------------------------------------------------------
# Tests: warning IS emitted
# ---------------------------------------------------------------------------


def test_warning_emitted_for_atmega32u4():
    """Warning is emitted when the MCU is ATmega32U4 (native USB CDC)."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("leonardo", "atmega32u4")

    mock_warn.assert_called_once()


def test_warning_emitted_for_atmega32u4_uppercase():
    """Warning is emitted regardless of MCU string case (ATMEGA32U4)."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("micro", "ATMEGA32U4")

    mock_warn.assert_called_once()


def test_warning_message_contains_board_id():
    """Warning message must identify the board so the user knows which env is affected."""
    board_id = "leonardo"
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc(board_id, "atmega32u4")

    warning_text = mock_warn.call_args[0][0]
    assert board_id in warning_text


def test_warning_message_mentions_usb_cdc():
    """Warning message must mention USB CDC so the user understands what Serial is."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("micro", "atmega32u4")

    warning_text = mock_warn.call_args[0][0]
    assert "USB" in warning_text


def test_warning_message_mentions_blocking():
    """Warning message must mention the blocking behaviour."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("micro", "atmega32u4")

    warning_text = mock_warn.call_args[0][0]
    assert "block" in warning_text.lower()


def test_warning_emitted_exactly_once():
    """_check_native_usb_cdc() must emit at most one warning per call."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("leonardo", "atmega32u4")

    assert mock_warn.call_count == 1


# ---------------------------------------------------------------------------
# Tests: warning is NOT emitted
# ---------------------------------------------------------------------------


def test_no_warning_for_atmega328p():
    """No warning when the MCU is ATmega328P (standard UART, no native USB)."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("uno", "atmega328p")

    mock_warn.assert_not_called()


def test_no_warning_for_atmega2560():
    """No warning when the MCU is ATmega2560 (standard UART, no native USB)."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("mega", "atmega2560")

    mock_warn.assert_not_called()


def test_no_warning_for_empty_mcu():
    """No warning when the MCU string is empty."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_avr.log_warning") as mock_warn:
        orch._check_native_usb_cdc("unknown_board", "")

    mock_warn.assert_not_called()
