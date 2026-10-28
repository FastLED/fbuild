//! Unit tests for [`super::EmergencyReserve`] (FastLED/fbuild#1750 item 6).
//!
//! The local object only: the process-wide reserve is exercised in
//! `tests/disk_reserve.rs`, because a second arming inside this binary would
//! race the `disk_full` unit tests that legitimately provoke releases.

use super::*;

/// A scratch directory under the crate's own target tree, so nothing lands in
/// `$TMPDIR` (`ban_unrooted_tempdir`).
fn scratch(label: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("disk-reserve-tests");
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(&base).unwrap_or_else(|err| panic!("scratch dir for {label}: {err}"))
}

#[test]
fn arming_commits_real_bytes_not_a_sparse_length() {
    let dir = scratch("armed");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    let reserve = EmergencyReserve::initialize_in(dir.path());
    assert_eq!(reserve.state(), ReserveState::Armed);
    assert_eq!(reserve.state().as_str(), "armed");
    assert_eq!(reserve.path(), path.as_path());
    assert_eq!(reserve.size(), EMERGENCY_RESERVE_BYTES);
    let metadata = std::fs::metadata(&path).expect("reserve file exists");
    assert_eq!(metadata.len(), EMERGENCY_RESERVE_BYTES);
}

#[test]
fn releasing_deletes_the_file_and_says_so_once() {
    let dir = scratch("released");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    let reserve = EmergencyReserve::initialize_in(dir.path());
    assert!(reserve.release("test release"));
    assert_eq!(reserve.state(), ReserveState::Released);
    assert!(!path.exists(), "released reserve must be gone");
    assert!(
        !reserve.release("again"),
        "a second release must be a no-op, not a second deletion"
    );
}

#[test]
fn a_reserve_deleted_behind_our_back_still_releases_cleanly() {
    let dir = scratch("vanished");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    let reserve = EmergencyReserve::initialize_in(dir.path());
    std::fs::remove_file(&path).expect("simulate an external cleanup");
    assert!(
        reserve.release("missing file"),
        "a missing file is tolerated and still transitions to released"
    );
    assert_eq!(reserve.state(), ReserveState::Released);
}

#[test]
fn only_a_full_volume_releases_the_reserve() {
    let dir = scratch("not-enospc");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    let reserve = EmergencyReserve::initialize_in(dir.path());
    let denied = io::Error::from(io::ErrorKind::PermissionDenied);
    assert!(!reserve.release_if_enospc(&denied, "unrelated write"));
    assert!(!reserve.release_if_full_message("undefined reference to `setup()'", "link"));
    assert_eq!(reserve.state(), ReserveState::Armed);
    assert!(path.exists());
}

#[test]
fn a_full_volume_from_a_tool_or_an_io_error_releases_it() {
    let dir = scratch("enospc");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    let reserve = EmergencyReserve::initialize_in(dir.path());
    assert!(reserve.release_if_full_message(
        "ld: final link failed: No space left on device",
        "ESP32 link"
    ));
    assert!(!path.exists());
    assert_eq!(reserve.state(), ReserveState::Released);

    let dir = scratch("enospc-io");
    let reserve = EmergencyReserve::initialize_in(dir.path());
    let code = if crate::platform::host::current().is_windows() {
        112
    } else {
        28
    };
    let full = io::Error::from_raw_os_error(code);
    assert!(reserve.release_if_enospc(&full, "writing firmware.bin"));
    assert_eq!(reserve.state(), ReserveState::Released);
}

#[test]
fn re_initialising_replaces_a_partial_file_from_a_crashed_run() {
    let dir = scratch("partial");
    let path = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    std::fs::write(&path, b"half").expect("partial file");
    let reserve = EmergencyReserve::initialize_in(dir.path());
    assert_eq!(reserve.state(), ReserveState::Armed);
    assert_eq!(
        std::fs::metadata(&path).expect("reserve").len(),
        EMERGENCY_RESERVE_BYTES,
        "a truncated reserve must be rebuilt, not trusted"
    );
}

#[test]
fn an_unallocatable_reserve_degrades_instead_of_failing() {
    let dir = scratch("degraded");
    // A directory where the reserve file should be makes creation fail the way
    // a full or read-only volume would.
    let blocked = dir.path().join(EMERGENCY_RESERVE_FILE_NAME);
    std::fs::create_dir(&blocked).expect("blocking directory");
    let reserve = EmergencyReserve::initialize_in(dir.path());
    assert_eq!(reserve.state(), ReserveState::Degraded);
    assert_eq!(reserve.state().as_str(), "degraded");
    assert!(
        !reserve.release("nothing to release"),
        "degraded has nothing to free"
    );
    assert!(blocked.exists());
}

#[test]
fn byte_counts_are_reported_in_mib() {
    assert_eq!(mib(EMERGENCY_RESERVE_BYTES), "32 MiB");
    assert_eq!(mib(0), "0 MiB");
}
