//! The process-wide emergency reserve, end to end (FastLED/fbuild#1750 item 6).
//!
//! The only test in this binary that touches the process-wide reserve, so the
//! arm/release/note cycle cannot race the `disk_full` unit tests (a different
//! process) or the other `it` modules here, which never build.

use fbuild_core::disk_full;
use fbuild_core::disk_reserve::{
    self, EMERGENCY_RESERVE_BYTES, EMERGENCY_RESERVE_FILE_NAME, ReserveState,
};

#[test]
fn an_armed_reserve_is_released_by_the_first_full_volume_and_re_armed_next_build() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("disk-reserve-tests");
    std::fs::create_dir_all(&base).expect("scratch base");
    let dir = tempfile::tempdir_in(&base).expect("scratch dir");
    let reserve_path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    assert_eq!(
        disk_reserve::process_state(),
        None,
        "nothing armed at start"
    );

    assert!(disk_reserve::arm_in(dir.path()), "first arm must arm");
    assert_eq!(disk_reserve::process_state(), Some(ReserveState::Armed));
    assert!(reserve_path.exists(), "armed reserve holds real space");
    assert_eq!(
        std::fs::metadata(&reserve_path).expect("reserve").len(),
        EMERGENCY_RESERVE_BYTES
    );
    assert!(
        !disk_reserve::arm_in(dir.path()),
        "a second build must not re-commit the reserve"
    );

    // An unrelated failure must not spend it.
    assert_eq!(
        disk_reserve::release_if_enospc(
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            "unrelated write"
        ),
        None
    );
    assert!(reserve_path.exists());

    // The full-volume failure releases it and says so in the message.
    let message = disk_full::disk_full_error("ESP32 link", "No space left on device").to_string();
    assert!(
        !reserve_path.exists(),
        "a full volume must free the reserve for the failure path"
    );
    assert_eq!(disk_reserve::process_state(), Some(ReserveState::Released));
    assert!(
        message.contains("released the 32 MiB emergency reserve"),
        "the user must be told what was freed: {message}"
    );
    assert!(
        message.contains("insufficient disk space"),
        "the actionable message is unchanged: {message}"
    );

    // A second failure has nothing left to release and must not claim otherwise.
    let second = disk_full::disk_full_error("ESP32 link", "No space left on device").to_string();
    assert!(
        !second.contains("emergency reserve"),
        "no reserve note when there was no reserve: {second}"
    );

    // The next build arms it again.
    assert!(
        disk_reserve::arm_in(dir.path()),
        "a released reserve is re-armed by the next build"
    );
    assert_eq!(disk_reserve::process_state(), Some(ReserveState::Armed));
    assert!(reserve_path.exists());
}
