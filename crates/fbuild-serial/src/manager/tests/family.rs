//! Pre-open board-family resolution tests (FastLED/fbuild#2083). Split from
//! `manager/tests.rs` to keep it under the 1000-LOC gate.

use super::super::*;

/// FastLED/fbuild#2083: the WebSocket attach resolves the board family
/// BEFORE starting its 3 s open deadline. The helper must be total and
/// bounded even for a port that cannot be identified.
#[tokio::test]
async fn resolve_open_family_is_total_and_bounded() {
    let mgr = SharedSerialManager::new();
    let family = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        mgr.resolve_open_family("__fbuild_missing_test_port__"),
    )
    .await
    .expect("family resolution must not stall");
    assert_eq!(family, crate::boards::BoardFamily::CdcAcmBridge);
}

/// FastLED/fbuild#2083: family inference runs outside the WebSocket open
/// cap, so it needs its own bound. A stalled `available_ports()` walk must
/// fall back at the bound instead of holding the attach.
#[tokio::test]
async fn resolve_family_bounded_falls_back_when_inference_stalls() {
    let started = std::time::Instant::now();
    let family = resolve_family_bounded(
        "COM_SLOW_ENUM",
        || {
            std::thread::sleep(Duration::from_millis(500));
            Some(crate::boards::BoardFamily::Esp32NativeUsbCdc)
        },
        Duration::from_millis(20),
        None,
    )
    .await;
    assert_eq!(family, crate::boards::BoardFamily::CdcAcmBridge);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "resolver must return at its bound, took {:?}",
        started.elapsed()
    );
}

/// The bound must not discard a family that arrives in time.
#[tokio::test]
async fn resolve_family_bounded_keeps_family_inferred_in_time() {
    let family = resolve_family_bounded(
        "COM_FAST_ENUM",
        || Some(crate::boards::BoardFamily::Esp32NativeUsbCdc),
        Duration::from_secs(5),
        None,
    )
    .await;
    assert_eq!(family, crate::boards::BoardFamily::Esp32NativeUsbCdc);
}

fn slow_native_usb(_: &str) -> Option<crate::boards::BoardFamily> {
    std::thread::sleep(Duration::from_millis(200));
    Some(crate::boards::BoardFamily::Esp32NativeUsbCdc)
}

/// A slow inference that finishes after the bound is still remembered, and
/// the next too-slow attach uses it instead of the `CdcAcmBridge` default,
/// whose DTR/RTS idle is wrong for a native-USB ESP32.
#[tokio::test]
async fn resolve_open_family_remembers_late_inference_for_next_timeout() {
    let mgr = SharedSerialManager::new();
    let bound = Duration::from_millis(20);
    let first = mgr
        .resolve_open_family_with("COM_LATE_ENUM", slow_native_usb, bound)
        .await;
    assert_eq!(first, crate::boards::BoardFamily::CdcAcmBridge);
    // Let the abandoned blocking resolver finish and record its answer.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let second = mgr
        .resolve_open_family_with("COM_LATE_ENUM", slow_native_usb, bound)
        .await;
    assert_eq!(second, crate::boards::BoardFamily::Esp32NativeUsbCdc);
}

fn unidentified(_: &str) -> Option<crate::boards::BoardFamily> {
    None
}

/// An unidentified port is an answer, not a failure: it must not reuse a
/// family remembered from a board that used to be on that port name.
#[tokio::test]
async fn resolve_open_family_forgets_family_when_port_is_unidentified() {
    let mgr = SharedSerialManager::new();
    let bound = Duration::from_secs(5);
    mgr.resolve_open_family_with("COM_SWAPPED", slow_native_usb, bound)
        .await;
    let family = mgr
        .resolve_open_family_with("COM_SWAPPED", unidentified, bound)
        .await;
    assert_eq!(family, crate::boards::BoardFamily::CdcAcmBridge);
}
