//! Unit tests for the parent `device_manager` module. Extracted to keep the
//! parent file under the 1000-LOC gate (see ci.yml LOC Gate workflow).

use super::*;

fn make_manager_with_device(port: &str) -> DeviceManager {
    let mgr = DeviceManager::new();
    mgr.insert_test_device(port);
    mgr
}

#[test]
fn new_manager_has_no_devices() {
    let mgr = DeviceManager::new();
    assert!(mgr.get_all_devices().is_empty());
}

#[test]
fn acquire_exclusive_succeeds() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_exclusive("COM3", "client-1", "testing", false)
        .unwrap();
    assert_eq!(lease.lease_type, LeaseType::Exclusive);
    assert_eq!(lease.client_id, "client-1");
}

#[test]
fn acquire_exclusive_twice_fails() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "client-1", "first", false)
        .unwrap();
    let result = mgr.acquire_exclusive("COM3", "client-2", "second", false);
    match result.unwrap_err() {
        DeviceLeaseError::ExclusiveConflict {
            port,
            device_id,
            holder,
            ..
        } => {
            assert_eq!(port, "COM3");
            assert_eq!(device_id, "1234:5678");
            assert_eq!(holder.client_id, "client-1");
            assert_eq!(holder.description, "first");
        }
        other => panic!("expected exclusive conflict, got {other:?}"),
    }
}

#[test]
fn acquire_monitor_succeeds() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_monitor("COM3", "client-1", "monitoring", false)
        .unwrap();
    assert_eq!(lease.lease_type, LeaseType::Monitor);
}

#[test]
fn multiple_monitor_leases_allowed() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_monitor("COM3", "client-1", "m1", false)
        .unwrap();
    mgr.acquire_monitor("COM3", "client-2", "m2", false)
        .unwrap();
    let state = mgr.get_device_status("COM3").unwrap();
    assert_eq!(state.monitor_count(), 2);
}

#[test]
fn release_lease_by_id() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_exclusive("COM3", "client-1", "test", false)
        .unwrap();
    mgr.release_lease(&lease.lease_id).unwrap();
    let state = mgr.get_device_status("COM3").unwrap();
    assert!(state.exclusive_lease.is_none());
}

#[test]
fn release_nonexistent_lease_fails() {
    let mgr = make_manager_with_device("COM3");
    assert!(mgr.release_lease("nonexistent").is_err());
}

#[test]
fn release_device_leases_clears_all() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "exc", false).unwrap();
    mgr.acquire_monitor("COM3", "c2", "mon", false).unwrap();
    let count = mgr.release_device_leases("COM3").unwrap();
    assert_eq!(count, 2); // 1 exclusive + 1 monitor
    let state = mgr.get_device_status("COM3").unwrap();
    assert!(state.exclusive_lease.is_none());
    assert_eq!(state.monitor_count(), 0);
}

#[test]
fn preempt_requires_reason() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "holder", false)
        .unwrap();
    let result = mgr.preempt_device("COM3", "c2", "");
    assert!(result.is_err());
    assert!(result.unwrap_err().message().contains("reason is required"));
}

#[test]
fn preempt_replaces_holder() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "original", false)
        .unwrap();
    let (lease, preempted) = mgr.preempt_device("COM3", "c2", "urgent deploy").unwrap();
    assert_eq!(lease.client_id, "c2");
    assert_eq!(lease.lease_type, LeaseType::Exclusive);
    assert_eq!(preempted.as_deref(), Some("c1"));
    // Old holder should be gone
    let state = mgr.get_device_status("COM3").unwrap();
    assert_eq!(state.exclusive_lease.as_ref().unwrap().client_id, "c2");
}

#[test]
fn device_not_found_errors() {
    let mgr = DeviceManager::new();
    assert!(mgr.acquire_exclusive("COM99", "c1", "x", false).is_err());
    assert!(mgr.acquire_monitor("COM99", "c1", "x", false).is_err());
    assert!(mgr.preempt_device("COM99", "c1", "reason").is_err());
}

#[test]
fn disconnected_device_rejects_leases() {
    let mgr = make_manager_with_device("COM3");
    {
        let mut devices = mgr.devices.lock().unwrap();
        devices.get_mut("COM3").unwrap().is_connected = false;
    }
    assert!(mgr.acquire_exclusive("COM3", "c1", "x", false).is_err());
    assert!(mgr.acquire_monitor("COM3", "c1", "x", false).is_err());
    assert!(mgr.preempt_device("COM3", "c1", "reason").is_err());
}

#[test]
fn cleanup_removes_disconnected_unlocked() {
    let mgr = make_manager_with_device("COM3");
    {
        let mut devices = mgr.devices.lock().unwrap();
        devices.get_mut("COM3").unwrap().is_connected = false;
    }
    assert_eq!(mgr.cleanup_stale_devices(), 1);
    assert!(mgr.get_all_devices().is_empty());
}

/// Calling `refresh_devices_if_stale` twice back-to-back with a
/// generous max-age must only actually run one OS-level
/// enumeration — the second call is inside the freshness window
/// and returns `false`. Regression guard for the sub-1 s warm
/// deploy path (#114 follow-up).
#[test]
fn refresh_devices_if_stale_skips_inside_window() {
    let mgr = DeviceManager::new();
    assert!(mgr.refresh_devices_if_stale(std::time::Duration::from_secs(5)));
    assert!(!mgr.refresh_devices_if_stale(std::time::Duration::from_secs(5)));
}

/// An already-stale refresh window must trigger a real
/// enumeration on the next call. `Duration::ZERO` is the
/// strictest case — any elapsed time is >= 0, so only an
/// in-flight call can short-circuit (and we don't have one).
#[test]
fn refresh_devices_if_stale_reruns_when_expired() {
    let mgr = DeviceManager::new();
    assert!(mgr.refresh_devices_if_stale(std::time::Duration::from_secs(5)));
    assert!(mgr.refresh_devices_if_stale(std::time::Duration::ZERO));
}

#[test]
fn tracked_serial_lease_moves_to_new_port_on_refresh() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "tracked deploy", true)
        .unwrap();

    mgr.refresh_from_discovered(vec![DiscoveredDevice {
        port: "COM4".to_string(),
        device_id: "1234:5678".to_string(),
        description: "Test Device Renumbered".to_string(),
        vid: Some(0x1234),
        pid: Some(0x5678),
        vendor_name: Some("Test Vendor".to_string()),
        product_name: Some("Test Device".to_string()),
        is_cdc: Some(true),
        serial_number: Some("TEST-SERIAL".to_string()),
        port_health: fbuild_serial::ports::PortHealth::HealthyPresent,
        instance_id: Some(r"USB\VID_1234&PID_5678\TEST-SERIAL".to_string()),
        parent_instance_id: Some(r"USB\VID_1234&PID_5678\PARENT".to_string()),
    }]);

    assert!(mgr.get_device_status("COM3").is_none());
    let moved = mgr.get_device_status("COM4").unwrap();
    assert_eq!(moved.previous_port.as_deref(), Some("COM3"));
    assert_eq!(moved.is_cdc, Some(true));
    assert_eq!(
        moved.port_health,
        fbuild_serial::ports::PortHealth::HealthyPresent
    );
    assert_eq!(
        moved.instance_id.as_deref(),
        Some(r"USB\VID_1234&PID_5678\TEST-SERIAL")
    );
    assert_eq!(
        moved.exclusive_lease.as_ref().map(|l| l.client_id.as_str()),
        Some("c1")
    );
    assert!(
        moved.exclusive_lease.as_ref().unwrap().track_serial,
        "moved lease must retain track_serial"
    );
    assert_eq!(
        mgr.take_recent_port_moves(),
        vec![DevicePortMove {
            previous_port: "COM3".to_string(),
            port: "COM4".to_string(),
            serial_number: Some("TEST-SERIAL".to_string()),
        }]
    );
    assert!(
        mgr.take_recent_port_moves().is_empty(),
        "taking recent moves should drain the queue"
    );
}

#[test]
fn untracked_serial_lease_stays_on_old_disconnected_port() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "untracked deploy", false)
        .unwrap();

    mgr.refresh_from_discovered(vec![DiscoveredDevice {
        port: "COM4".to_string(),
        device_id: "1234:5678".to_string(),
        description: "Test Device Renumbered".to_string(),
        vid: Some(0x1234),
        pid: Some(0x5678),
        vendor_name: Some("Test Vendor".to_string()),
        product_name: Some("Test Device".to_string()),
        is_cdc: Some(false),
        serial_number: Some("TEST-SERIAL".to_string()),
        port_health: fbuild_serial::ports::PortHealth::PresentProblem {
            problem_code: 31,
            status: Some(0),
        },
        instance_id: Some(r"USB\VID_1234&PID_5678\TEST-SERIAL".to_string()),
        parent_instance_id: Some(r"USB\VID_1234&PID_5678\PARENT".to_string()),
    }]);

    let old = mgr.get_device_status("COM3").unwrap();
    assert!(!old.is_connected);
    assert!(old.exclusive_lease.is_some());
    let new = mgr.get_device_status("COM4").unwrap();
    assert!(new.exclusive_lease.is_none());
    assert_eq!(new.is_cdc, Some(false));
}

#[test]
fn trusted_hash_round_trip() {
    let mgr = make_manager_with_device("COM3");
    assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
    let h = [7u8; 32];
    mgr.set_trusted_firmware_hash("COM3", h);
    assert_eq!(mgr.trusted_firmware_hash("COM3"), Some(h));
}

#[test]
fn trusted_hash_cleared_on_demand() {
    let mgr = make_manager_with_device("COM3");
    mgr.set_trusted_firmware_hash("COM3", [1u8; 32]);
    mgr.clear_trusted_firmware_hash("COM3");
    assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
}

/// Unknown port is never trusted — no panic, no fabrication of
/// device state. Regression guard: the deploy handler calls this
/// with a user-supplied port string that may not be in the
/// daemon's enumeration cache yet.
#[test]
fn trusted_hash_unknown_port_is_none() {
    let mgr = DeviceManager::new();
    assert_eq!(mgr.trusted_firmware_hash("COM99"), None);
}

/// A disconnected device must never be trusted, even if the hash
/// was previously recorded. Re-enumeration can come back with a
/// physically different board on the same port name — trust
/// across that boundary is unsafe.
#[test]
fn trusted_hash_invalid_after_disconnect() {
    let mgr = make_manager_with_device("COM3");
    mgr.set_trusted_firmware_hash("COM3", [9u8; 32]);
    // Simulate a disconnect that happened *after* the trust was
    // recorded — exactly the condition
    // `trusted_firmware_hash` must treat as "unsafe to trust".
    {
        let mut devices = mgr.devices.lock().unwrap();
        let state = devices.get_mut("COM3").unwrap();
        state.is_connected = false;
        state.last_disconnect_at = Some(Instant::now());
    }
    assert_eq!(mgr.trusted_firmware_hash("COM3"), None);
}

/// A stale disconnect stamp from *before* the trust was set
/// (e.g. device was unplugged earlier in the session but the
/// user reconnected and we re-trusted) does NOT invalidate the
/// fresh trust. Ordering is what matters: `disconnect > set` ⇒
/// untrusted; `set > disconnect` ⇒ trusted.
#[test]
fn trusted_hash_survives_older_disconnect_stamp() {
    let mgr = make_manager_with_device("COM3");
    // Plant an old disconnect stamp first, then re-set trust
    // later (what happens on a re-enumerate + fresh deploy).
    {
        let mut devices = mgr.devices.lock().unwrap();
        let state = devices.get_mut("COM3").unwrap();
        state.last_disconnect_at = Some(Instant::now());
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    mgr.set_trusted_firmware_hash("COM3", [3u8; 32]);
    assert_eq!(mgr.trusted_firmware_hash("COM3"), Some([3u8; 32]));
}

#[test]
fn cleanup_preserves_leased_disconnected() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "c1", "deploy", false)
        .unwrap();
    {
        let mut devices = mgr.devices.lock().unwrap();
        devices.get_mut("COM3").unwrap().is_connected = false;
    }
    assert_eq!(mgr.cleanup_stale_devices(), 0);
    assert_eq!(mgr.get_all_devices().len(), 1);
}
