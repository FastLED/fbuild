//! Device reset via serial port DTR/RTS toggling.
//!
//! Platform-specific reset sequences:
//! - Teensy: Open at 134 baud (magic reboot trigger)
//! - ESP32: DTR/RTS sequence (same as esptool)
//! - AVR/Generic: DTR toggle

use fbuild_core::Result;
use std::time::Duration;

/// Reset a device on the given serial port using a platform-appropriate sequence.
///
/// `platform` should be one of: "teensy", "esp32", "avr", "generic".
pub fn reset_device(platform: &str, port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("resetting {} device on {}", platform, port);
    }

    let result = match platform {
        "teensy" => reset_teensy(port, verbose),
        "esp32" => reset_esp32(port, verbose),
        "avr" => reset_avr(port, verbose),
        _ => reset_generic(port, verbose),
    };

    match &result {
        Ok(true) => {
            if verbose {
                tracing::info!("device reset successful");
            }
        }
        Ok(false) => tracing::warn!("device reset reported failure"),
        Err(e) => tracing::error!("device reset error: {}", e),
    }

    result
}

/// Detect platform string from a board identifier (from platformio.ini).
pub fn detect_platform_for_reset(board: &str) -> &'static str {
    let board_lower = board.to_lowercase();
    if board_lower.starts_with("teensy") {
        "teensy"
    } else if board_lower.starts_with("esp32") {
        "esp32"
    } else if board_lower == "uno"
        || board_lower == "nano"
        || board_lower == "mega"
        || board_lower.starts_with("atmega")
        || board_lower.starts_with("arduino")
    {
        "avr"
    } else {
        "generic"
    }
}

/// Teensy reset: open serial at 134 baud (magic reboot baud rate).
fn reset_teensy(port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("teensy reset: opening {} at 134 baud", port);
    }

    let _serial = serialport::new(port, 134)
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| {
            fbuild_core::FbuildError::SerialError(format!("failed to open {}: {}", port, e))
        })?;

    // Hold open briefly — the Teensy bootloader triggers reboot on 134 baud connection
    std::thread::sleep(Duration::from_millis(100));
    // Port drops here, closing the connection

    Ok(true)
}

/// ESP32 reset: DTR/RTS sequence (same as esptool boot sequence).
fn reset_esp32(port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("esp32 reset: DTR/RTS sequence on {}", port);
    }

    let mut serial = serialport::new(port, 115_200)
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| {
            fbuild_core::FbuildError::SerialError(format!("failed to open {}: {}", port, e))
        })?;

    // EN pin low (reset)
    serial
        .write_data_terminal_ready(false)
        .map_err(map_serial)?;
    serial.write_request_to_send(true).map_err(map_serial)?;
    std::thread::sleep(Duration::from_millis(100));

    // EN pin high (release reset)
    serial.write_data_terminal_ready(true).map_err(map_serial)?;
    serial.write_request_to_send(false).map_err(map_serial)?;
    std::thread::sleep(Duration::from_millis(50));

    // Final state
    serial
        .write_data_terminal_ready(false)
        .map_err(map_serial)?;

    Ok(true)
}

/// AVR reset: DTR toggle at 115200 baud.
fn reset_avr(port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("avr reset: DTR toggle on {}", port);
    }

    let mut serial = serialport::new(port, 115_200)
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| {
            fbuild_core::FbuildError::SerialError(format!("failed to open {}: {}", port, e))
        })?;

    serial
        .write_data_terminal_ready(false)
        .map_err(map_serial)?;
    std::thread::sleep(Duration::from_millis(100));
    serial.write_data_terminal_ready(true).map_err(map_serial)?;
    std::thread::sleep(Duration::from_millis(100));

    Ok(true)
}

/// Generic reset: same as AVR (DTR toggle).
fn reset_generic(port: &str, verbose: bool) -> Result<bool> {
    if verbose {
        tracing::info!("generic reset: DTR toggle on {}", port);
    }
    reset_avr(port, verbose)
}

fn map_serial(e: serialport::Error) -> fbuild_core::FbuildError {
    fbuild_core::FbuildError::SerialError(format!("serial control error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_platform_for_reset() {
        assert_eq!(detect_platform_for_reset("teensy40"), "teensy");
        assert_eq!(detect_platform_for_reset("teensy41"), "teensy");
        assert_eq!(detect_platform_for_reset("esp32dev"), "esp32");
        assert_eq!(detect_platform_for_reset("esp32-c3"), "esp32");
        assert_eq!(detect_platform_for_reset("uno"), "avr");
        assert_eq!(detect_platform_for_reset("nano"), "avr");
        assert_eq!(detect_platform_for_reset("mega"), "avr");
        assert_eq!(detect_platform_for_reset("atmega328p"), "avr");
        assert_eq!(detect_platform_for_reset("unknown_board"), "generic");
    }
}
