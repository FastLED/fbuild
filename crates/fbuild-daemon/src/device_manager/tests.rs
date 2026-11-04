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
        location_paths: vec!["PCIROOT(0)#USBROOT(0)#USB(3)".to_string()],
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
fn phantom_refresh_retains_last_nonempty_usb_location_paths() {
    let mgr = DeviceManager::new();
    let discovered = |health, location_paths| DiscoveredDevice {
        port: "COM18".to_string(),
        device_id: "2e8a:f00f".to_string(),
        description: "Raspberry Pi Pico 2 W".to_string(),
        vid: Some(0x2e8a),
        pid: Some(0xf00f),
        vendor_name: Some("Raspberry Pi".to_string()),
        product_name: Some("Pico 2".to_string()),
        is_cdc: Some(true),
        serial_number: Some("2DCB876B587EA334".to_string()),
        port_health: health,
        instance_id: Some("USB\\VID_2E8A&PID_F00F\\2DCB876B587EA334".to_string()),
        parent_instance_id: Some("USB\\ROOT_HUB30\\parent".to_string()),
        location_paths,
    };
    let location = "PCIROOT(0)#USBROOT(0)#USB(10)#USB(4)#USBMI(0)".to_string();
    mgr.refresh_from_discovered(vec![discovered(
        fbuild_serial::ports::PortHealth::HealthyPresent,
        vec![location.clone()],
    )]);
    mgr.refresh_from_discovered(vec![discovered(
        fbuild_serial::ports::PortHealth::Phantom {
            problem_code: None,
            status: None,
        },
        Vec::new(),
    )]);

    assert_eq!(
        mgr.get_device_status("COM18").unwrap().location_paths,
        vec![location]
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
        location_paths: Vec::new(),
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

#[test]
fn admit_allows_an_unleased_port() {
    let mgr = make_manager_with_device("COM3");
    mgr.admit_operation("COM3", None).unwrap();
}

#[test]
fn admit_refuses_an_operation_that_presents_no_lease() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_exclusive("COM3", "client-a", "hardware validation", true)
        .unwrap();
    match mgr.admit_operation("COM3", None).unwrap_err() {
        DeviceLeaseError::ForeignLeaseHeld { port, holder } => {
            assert_eq!(port, "COM3");
            assert_eq!(holder.lease_id, lease.lease_id);
            assert_eq!(holder.client_id, "client-a");
        }
        other => panic!("expected a foreign-lease refusal, got {other:?}"),
    }
}

#[test]
fn admit_refuses_an_operation_that_presents_a_different_lease() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_exclusive("COM3", "client-a", "validation", false)
        .unwrap();
    let err = mgr
        .admit_operation("COM3", Some("not-the-lease"))
        .unwrap_err();
    assert!(err.message().contains("reserved by exclusive lease"));
}

#[test]
fn admit_allows_the_lease_holder() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_exclusive("COM3", "client-a", "validation", true)
        .unwrap();
    mgr.admit_operation("COM3", Some(&lease.lease_id)).unwrap();
}

#[test]
fn admit_does_not_block_on_monitor_leases() {
    let mgr = make_manager_with_device("COM3");
    mgr.acquire_monitor("COM3", "client-b", "watching", false)
        .unwrap();
    mgr.admit_operation("COM3", None).unwrap();
}

#[test]
fn foreign_lease_message_names_the_lease_and_the_escape_hatches() {
    let mgr = make_manager_with_device("COM3");
    let lease = mgr
        .acquire_exclusive("COM3", "client-a", "validation", false)
        .unwrap();
    let message = mgr.admit_operation("COM3", None).unwrap_err().message();
    assert!(message.contains(&lease.lease_id), "{message}");
    assert!(message.contains("--lease-id"), "{message}");
    assert!(message.contains("fbuild device release COM3"), "{message}");
}

#[test]
fn admit_ignores_ports_the_manager_does_not_know() {
    let mgr = DeviceManager::new();
    mgr.admit_operation("COM99", None).unwrap();
}

mod lease_wait_queue_tests {
    //! Opt-in bounded wait for a foreign exclusive lease
    //! ([`DeviceManager::await_admission`], FastLED/fbuild#1864 criterion 3).
    //!
    //! A competing workflow joins a per-port FIFO queue instead of being
    //! refused; ordering is deterministic (head of queue only), the wait
    //! happens before any preemption/open, and a timed-out or cancelled
    //! waiter leaves the queue so the next workflow is promoted.
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    const PORT: &str = "COM7";

    fn manager_with_holder() -> (Arc<DeviceManager>, String) {
        let mgr = Arc::new(DeviceManager::new());
        mgr.insert_test_device(PORT);
        let lease = mgr
            .acquire_exclusive(PORT, "client-a", "hardware validation", false)
            .unwrap();
        (mgr, lease.lease_id)
    }

    fn waiting_position(poll: AdmissionPoll) -> usize {
        match poll {
            AdmissionPoll::Waiting { position, holder } => {
                assert!(holder.is_none(), "no lease is held in these cases");
                position
            }
            AdmissionPoll::Admitted => panic!("waiter was admitted out of order"),
        }
    }

    #[test]
    fn queue_is_fifo_and_admits_only_at_the_head() {
        let mgr = make_manager_with_device(PORT);
        let first = mgr.enqueue_waiter(PORT, "client-a");
        let second = mgr.enqueue_waiter(PORT, "client-b");
        assert!(mgr.poll_admission(PORT, None, Some(first)).is_admitted());
        assert_eq!(
            waiting_position(mgr.poll_admission(PORT, None, Some(second))),
            2
        );
        // Diagnostics name the waiting workflows in queue order.
        assert_eq!(mgr.queued_client_ids(PORT), vec!["client-a", "client-b"]);
        // Dropping the head promotes the next workflow.
        mgr.leave_queue(PORT, first);
        assert!(mgr.poll_admission(PORT, None, Some(second)).is_admitted());
        mgr.leave_queue(PORT, second);
        assert_eq!(mgr.queued_workflows(PORT), 0);
    }

    #[test]
    fn a_foreign_lease_holds_the_head_and_names_the_holder() {
        let (mgr, lease_id) = manager_with_holder();
        let ticket = mgr.enqueue_waiter(PORT, "client-b");
        match mgr.poll_admission(PORT, None, Some(ticket)) {
            AdmissionPoll::Waiting { position, holder } => {
                assert_eq!(position, 1);
                let holder = holder.expect("the foreign lease must be reported");
                assert_eq!(holder.lease_id, lease_id);
                assert_eq!(holder.client_id, "client-a");
            }
            AdmissionPoll::Admitted => panic!("a foreign lease admitted the waiter"),
        }
        mgr.release_lease(&lease_id).unwrap();
        assert!(mgr.poll_admission(PORT, None, Some(ticket)).is_admitted());
        mgr.leave_queue(PORT, ticket);
    }

    #[test]
    fn an_unqueued_caller_does_not_jump_a_waiting_workflow() {
        let mgr = make_manager_with_device(PORT);
        let ticket = mgr.enqueue_waiter(PORT, "client-a");
        // A fail-fast caller (no ticket) sees the queue, so two clients that
        // race a release cannot reorder: it would join *behind* the waiter.
        assert_eq!(waiting_position(mgr.poll_admission(PORT, None, None)), 2);
        mgr.leave_queue(PORT, ticket);
        assert!(mgr.poll_admission(PORT, None, None).is_admitted());
    }

    #[test]
    fn unknown_ports_stay_outside_the_lease_path() {
        let mgr = make_manager_with_device(PORT);
        assert!(mgr.poll_admission("/dev/ttyNOPE", None, None).is_admitted());
    }

    #[tokio::test]
    async fn bounded_wait_admits_after_the_holder_releases() {
        let (mgr, lease_id) = manager_with_holder();
        let releaser = Arc::clone(&mgr);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            releaser.release_lease(&lease_id).unwrap();
        });
        let waited = mgr
            .await_admission(PORT, None, 5_000, "client-b")
            .await
            .unwrap();
        assert!(
            waited >= 100,
            "admitted before the holder released: {waited} ms"
        );
        assert_eq!(mgr.queued_workflows(PORT), 0);
    }

    #[tokio::test]
    async fn bounded_wait_times_out_and_leaves_the_queue() {
        let (mgr, lease_id) = manager_with_holder();
        let err = mgr
            .await_admission(PORT, None, 140, "client-b")
            .await
            .unwrap_err();
        let message = err.message();
        assert!(message.contains(&lease_id), "{message}");
        assert!(message.contains("not reset"), "{message}");
        // The abandoned ticket must not block the next workflow.
        assert_eq!(mgr.queued_workflows(PORT), 0);
    }

    #[tokio::test]
    async fn zero_wait_is_still_fail_fast() {
        let (mgr, _lease_id) = manager_with_holder();
        let err = mgr
            .await_admission(PORT, None, 0, "client-b")
            .await
            .unwrap_err();
        assert!(err.message().contains("--lease-wait"), "{}", err.message());
        assert_eq!(mgr.queued_workflows(PORT), 0);
    }

    #[tokio::test]
    async fn the_holder_waits_for_nobody() {
        let (mgr, lease_id) = manager_with_holder();
        let _queued = mgr.enqueue_waiter(PORT, "client-b");
        let waited = mgr
            .await_admission(PORT, Some(&lease_id), 5_000, "client-a")
            .await
            .unwrap();
        assert_eq!(waited, 0);
    }
}

mod lease_renumber_tests {
    //! Lease identity across a tracked USB re-enumeration
    //! (FastLED/fbuild#1864 criterion 4).
    //!
    //! `track_serial` moves the [`DeviceState`] — and the exclusive lease with
    //! it — to the new port name. Everything else keys on the port string, so
    //! the admission queue, the `QueueGuard` and a caller still holding the
    //! stale name must follow the board instead of silently passing the lease
    //! check on a port that no longer exists.
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    const OLD_PORT: &str = "COM7";
    const NEW_PORT: &str = "COM8";

    fn renumbered_device() -> DiscoveredDevice {
        DiscoveredDevice {
            port: NEW_PORT.to_string(),
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
            location_paths: vec!["PCIROOT(0)#USBROOT(0)#USB(3)".to_string()],
        }
    }

    fn manager_with_tracked_holder() -> (Arc<DeviceManager>, String) {
        let mgr = Arc::new(DeviceManager::new());
        mgr.insert_test_device(OLD_PORT);
        let lease = mgr
            .acquire_exclusive(OLD_PORT, "client-a", "hardware validation", true)
            .unwrap();
        (mgr, lease.lease_id)
    }

    fn renumber(mgr: &DeviceManager) {
        mgr.refresh_from_discovered(vec![renumbered_device()]);
    }

    #[test]
    fn tracked_renumber_moves_the_admission_queue() {
        let (mgr, lease_id) = manager_with_tracked_holder();
        let ticket = mgr.enqueue_waiter(OLD_PORT, "client-b");
        renumber(&mgr);

        assert_eq!(
            mgr.queued_workflows(OLD_PORT),
            0,
            "the old port is gone; its queue must not linger"
        );
        assert_eq!(mgr.queued_client_ids(NEW_PORT), vec!["client-b"]);
        assert!(
            matches!(
                mgr.poll_admission(NEW_PORT, None, Some(ticket)),
                AdmissionPoll::Waiting {
                    position: 1,
                    holder: Some(_)
                }
            ),
            "the waiter must still queue behind the migrated holder"
        );
        mgr.release_lease(&lease_id).unwrap();
        assert!(
            mgr.poll_admission(NEW_PORT, None, Some(ticket))
                .is_admitted()
        );
        mgr.leave_ticket(ticket);
        assert_eq!(mgr.queued_workflows(NEW_PORT), 0);
    }

    #[test]
    fn a_stale_port_name_reports_the_renumbered_port() {
        let (mgr, lease_id) = manager_with_tracked_holder();
        renumber(&mgr);

        let err = mgr.admit_operation(OLD_PORT, None).unwrap_err();
        match &err {
            DeviceLeaseError::PortRenumbered {
                port,
                current_port,
                holder,
            } => {
                assert_eq!(port, OLD_PORT);
                assert_eq!(current_port, NEW_PORT);
                assert_eq!(
                    holder.as_deref().map(|lease| lease.lease_id.as_str()),
                    Some(lease_id.as_str())
                );
            }
            other => panic!("expected a renumber report, got {other:?}"),
        }
        let message = err.message();
        assert!(message.contains(NEW_PORT), "{message}");
        assert!(message.contains("re-enumerated"), "{message}");

        // The holder (and only the holder) may operate on the new name.
        mgr.admit_operation(NEW_PORT, Some(&lease_id)).unwrap();
        assert!(matches!(
            mgr.admit_operation(NEW_PORT, None),
            Err(DeviceLeaseError::ForeignLeaseHeld { .. })
        ));
    }

    #[tokio::test]
    async fn a_queued_workflow_follows_the_board_across_a_renumber() {
        let (mgr, lease_id) = manager_with_tracked_holder();
        let waiter = {
            let mgr = Arc::clone(&mgr);
            tokio::spawn(
                async move { mgr.await_admission(OLD_PORT, None, 5_000, "client-b").await },
            )
        };
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            mgr.queued_workflows(OLD_PORT),
            1,
            "the workflow must be queued before the renumber"
        );

        renumber(&mgr);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !waiter.is_finished(),
            "a waiting workflow was admitted on a port that no longer exists"
        );
        assert_eq!(
            mgr.queued_workflows(NEW_PORT),
            1,
            "the queue must follow the board"
        );

        mgr.release_lease(&lease_id).unwrap();
        let waited = waiter.await.unwrap().unwrap();
        assert!(
            waited >= 200,
            "admitted before the holder released: {waited} ms"
        );
        assert_eq!(
            mgr.queued_workflows(NEW_PORT),
            0,
            "the guard must remove the ticket from the migrated queue"
        );
    }
}
