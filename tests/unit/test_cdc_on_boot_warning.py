"""
Unit tests for the CDC on boot warning emitted by OrchestratorESP32.

Tests verify that OrchestratorESP32._check_cdc_on_boot() emits a log_warning
when ARDUINO_USB_CDC_ON_BOOT=1 is the effective value in the combined board-JSON
and user build-flags, and that the warning is suppressed when CDC on boot is
disabled or not configured.
"""

from unittest.mock import call, patch

from fbuild.build.orchestrator_esp32 import OrchestratorESP32
from fbuild.packages import Cache

# ---------------------------------------------------------------------------
# Mock board JSON fixtures
# ---------------------------------------------------------------------------

# Board that enables CDC on boot (e.g. Adafruit Feather ESP32-S3)
BOARD_JSON_CDC_ENABLED = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": [
            "-DARDUINO_ADAFRUIT_FEATHER_ESP32S3",
            "-DARDUINO_USB_CDC_ON_BOOT=1",
        ],
    },
    "name": "Adafruit Feather ESP32-S3",
}

# Board that explicitly disables CDC on boot (e.g. freenove_esp32_s3_wroom)
BOARD_JSON_CDC_DISABLED = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": [
            "-DARDUINO_FREENOVE_ESP32_S3_WROOM",
            "-DARDUINO_USB_CDC_ON_BOOT=0",
        ],
    },
    "name": "Freenove ESP32-S3-WROOM",
}

# Board with no CDC on boot flag at all (e.g. plain ESP32 dev board)
BOARD_JSON_NO_CDC_FLAG = {
    "build": {
        "mcu": "esp32",
        "extra_flags": ["-DARDUINO_ESP32_DEV"],
    },
    "name": "ESP32 Dev Module",
}

# Board that carries CDC_ON_BOOT in the nested build.arduino.extra_flags
BOARD_JSON_CDC_NESTED = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": [],
        "arduino": {
            "extra_flags": ["-DARDUINO_USB_CDC_ON_BOOT=1"],
        },
    },
    "name": "Nested-flags ESP32-S3 board",
}

# Board where the flag is provided as a space-separated string instead of a list
BOARD_JSON_CDC_STRING_FLAGS = {
    "build": {
        "mcu": "esp32s3",
        "extra_flags": "-DARDUINO_SOME_BOARD -DARDUINO_USB_CDC_ON_BOOT=1",
    },
    "name": "String-flags ESP32-S3 board",
}


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _make_orch() -> OrchestratorESP32:
    return OrchestratorESP32(Cache(), verbose=False)


# ---------------------------------------------------------------------------
# Tests: method existence
# ---------------------------------------------------------------------------


def test_check_cdc_on_boot_method_exists():
    """_check_cdc_on_boot() must exist and be callable on OrchestratorESP32."""
    orch = _make_orch()
    assert hasattr(orch, "_check_cdc_on_boot")
    assert callable(orch._check_cdc_on_boot)


# ---------------------------------------------------------------------------
# Tests: warning IS emitted
# ---------------------------------------------------------------------------


def test_warning_emitted_when_cdc_enabled_in_board_json():
    """Warning is emitted when board JSON has ARDUINO_USB_CDC_ON_BOOT=1."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("adafruit_feather_esp32s3", [], BOARD_JSON_CDC_ENABLED)

    mock_warn.assert_called_once()
    warning_text = mock_warn.call_args[0][0]
    assert "CDC" in warning_text
    assert "ARDUINO_USB_CDC_ON_BOOT" in warning_text


def test_warning_message_contains_board_id():
    """Warning message must identify the board so the user knows which env is affected."""
    board_id = "adafruit_feather_esp32s3"
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot(board_id, [], BOARD_JSON_CDC_ENABLED)

    warning_text = mock_warn.call_args[0][0]
    assert board_id in warning_text


def test_warning_emitted_for_nested_extra_flags():
    """Warning fires when CDC flag lives in build.arduino.extra_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("nested_board", [], BOARD_JSON_CDC_NESTED)

    mock_warn.assert_called_once()


def test_warning_emitted_for_string_extra_flags():
    """Warning fires when board JSON extra_flags is a space-separated string."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("string_flags_board", [], BOARD_JSON_CDC_STRING_FLAGS)

    mock_warn.assert_called_once()


def test_warning_emitted_when_user_enables_cdc_via_build_flags():
    """Warning fires when the user enables CDC on boot through platformio.ini build_flags."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot(
            "esp32-s3-devkitc-1",
            ["-DARDUINO_USB_CDC_ON_BOOT=1"],  # user-added flag
            BOARD_JSON_NO_CDC_FLAG,
        )

    mock_warn.assert_called_once()


# ---------------------------------------------------------------------------
# Tests: warning is NOT emitted
# ---------------------------------------------------------------------------


def test_no_warning_when_no_cdc_flag():
    """No warning when ARDUINO_USB_CDC_ON_BOOT is absent from all flag sources."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("esp32dev", [], BOARD_JSON_NO_CDC_FLAG)

    mock_warn.assert_not_called()


def test_no_warning_when_cdc_explicitly_disabled_in_board_json():
    """No warning when board JSON explicitly sets ARDUINO_USB_CDC_ON_BOOT=0."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("freenove_esp32_s3_wroom", [], BOARD_JSON_CDC_DISABLED)

    mock_warn.assert_not_called()


def test_no_warning_when_user_disables_cdc_after_board_enables_it():
    """User can suppress the warning by adding -DARDUINO_USB_CDC_ON_BOOT=0 to build_flags.

    The user's build_flags are applied after board JSON extra_flags, so the last
    definition wins (C preprocessor semantics).
    """
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot(
            "adafruit_feather_esp32s3",
            ["-DARDUINO_USB_CDC_ON_BOOT=0"],  # user override disables CDC
            BOARD_JSON_CDC_ENABLED,
        )

    mock_warn.assert_not_called()


def test_last_definition_wins_user_enables_over_board_disable():
    """If the board disables CDC but user re-enables it, the warning is emitted.

    The last flag in the combined sequence takes effect, matching C preprocessor
    behaviour where later -D definitions override earlier ones.
    """
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot(
            "freenove_esp32_s3_wroom",
            ["-DARDUINO_USB_CDC_ON_BOOT=1"],  # user explicitly re-enables
            BOARD_JSON_CDC_DISABLED,
        )

    mock_warn.assert_called_once()


def test_warning_emitted_exactly_once_not_multiple_times():
    """_check_cdc_on_boot must emit at most one warning per call."""
    orch = _make_orch()
    with patch("fbuild.build.orchestrator_esp32.log_warning") as mock_warn:
        orch._check_cdc_on_boot("adafruit_feather_esp32s3", [], BOARD_JSON_CDC_ENABLED)

    assert mock_warn.call_count == 1
