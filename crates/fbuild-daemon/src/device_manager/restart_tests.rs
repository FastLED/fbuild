//! Exclusive lease restoration against a real, isolated snapshot directory.

use super::*;

/// FastLED/fbuild#2150
#[tokio::test]
async fn tracked_exclusive_lease_survives_manager_restart() {
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let lease = manager
        .acquire_exclusive("COM3", "holder", "test workflow", true)
        .unwrap();
    manager
        .acquire_monitor("COM3", "monitor", "test stream", true)
        .unwrap();
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    assert!(manager.admit_operation("COM3", None).is_err());
    assert!(
        manager
            .admit_operation("COM3", Some(&lease.lease_id))
            .is_ok()
    );
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let status = manager.get_device_status("COM3").unwrap();
    assert!(!status.is_available_for_exclusive());
    assert_eq!(status.monitor_count(), 0);
    assert!(
        manager
            .admit_operation("COM3", Some(&lease.lease_id))
            .is_ok()
    );
    assert!(manager.admit_operation("COM3", None).is_err());
    assert!(matches!(
        manager.acquire_exclusive("COM3", "competitor", "", true),
        Err(DeviceLeaseError::ExclusiveConflict { .. })
    ));

    manager.release_lease(&lease.lease_id).unwrap();
    drop(manager);
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    assert!(
        manager
            .get_device_status("COM3")
            .unwrap()
            .is_available_for_exclusive()
    );
    assert!(manager.release_lease(&lease.lease_id).is_err());
}

#[tokio::test]
async fn tracked_exclusive_lease_follows_usb_renumber_after_restart() {
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let lease = manager
        .acquire_exclusive("COM3", "holder", "test workflow", true)
        .unwrap();
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    let mut replacement = usb_device("COM3");
    replacement.serial_number = Some("OTHER-DEVICE".to_string());
    manager.refresh_from_discovered(vec![replacement, usb_device("COM4")]);
    assert!(
        manager
            .get_device_status("COM3")
            .unwrap()
            .is_available_for_exclusive()
    );
    let moved = manager.get_device_status("COM4").unwrap();
    assert_eq!(moved.previous_port.as_deref(), Some("COM3"));
    assert!(
        manager
            .admit_operation("COM4", Some(&lease.lease_id))
            .is_ok()
    );
    assert!(manager.admit_operation("COM4", None).is_err());
    assert_eq!(manager.current_port_for("COM3"), "COM3");
    assert!(manager.take_recent_port_moves().is_empty());
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM5")]);
    assert_eq!(manager.current_port_for("COM4"), "COM5");
    assert!(manager.admit_operation("COM4", None).is_err());
    assert!(
        manager
            .admit_operation("COM5", Some(&lease.lease_id))
            .is_ok()
    );
    manager.release_lease(&lease.lease_id).unwrap();
}

#[tokio::test]
async fn exact_saved_port_wins_over_off_port_duplicate_usb_serial() {
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3"), usb_device("COM4")]);
    let absent = manager
        .acquire_exclusive("COM3", "absent", "", true)
        .unwrap();
    let present = manager
        .acquire_exclusive("COM4", "present", "", true)
        .unwrap();
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM4")]);
    assert!(
        manager
            .admit_operation("COM4", Some(&present.lease_id))
            .is_ok()
    );
    assert!(
        manager
            .admit_operation("COM4", Some(&absent.lease_id))
            .is_err()
    );
}

#[tokio::test]
async fn expired_exclusive_lease_does_not_restore_after_restart() {
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let lease = manager
        .acquire_exclusive_with_ttl("COM3", "holder", "", true, std::num::NonZeroU32::new(60))
        .unwrap();
    assert_eq!(lease.expires_at, Some(lease.acquired_at + 60.0));
    drop(manager);

    // Move the saved deadline into the past so the test needs no wall-clock wait.
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    snapshot[0]["lease"]["expires_at"] = serde_json::json!(1);
    std::fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    assert!(
        manager
            .get_device_status("COM3")
            .unwrap()
            .is_available_for_exclusive()
    );
    let replacement = manager
        .acquire_exclusive("COM3", "competitor", "", true)
        .unwrap();
    assert!(
        manager
            .admit_operation("COM3", Some(&lease.lease_id))
            .is_err()
    );
    assert!(
        manager
            .admit_operation("COM3", Some(&replacement.lease_id))
            .is_ok()
    );
}

#[tokio::test]
async fn failed_exclusive_lease_write_preserves_current_owner() {
    let cache = tempfile::tempdir().unwrap();
    let directory = cache.path().join("daemon");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let lease = manager
        .acquire_exclusive("COM3", "holder", "", true)
        .unwrap();
    std::fs::rename(&directory, cache.path().join("daemon-away")).unwrap();

    assert!(manager.release_lease(&lease.lease_id).is_err());
    assert!(
        manager
            .preempt_device("COM3", "competitor", "take")
            .is_err()
    );
    assert!(
        manager
            .admit_operation("COM3", Some(&lease.lease_id))
            .is_ok()
    );
    assert!(manager.admit_operation("COM3", None).is_err());
}

#[tokio::test]
async fn exclusive_takeover_and_release_all_persist_across_restart() {
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("device-leases.json");
    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    let original = manager
        .acquire_exclusive("COM3", "original", "", true)
        .unwrap();
    let (replacement, _) = manager
        .preempt_device("COM3", "replacement", "take")
        .unwrap();
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    assert!(
        manager
            .admit_operation("COM3", Some(&original.lease_id))
            .is_err()
    );
    assert!(manager.release_lease(&original.lease_id).is_err());
    assert!(
        manager
            .admit_operation("COM3", Some(&replacement.lease_id))
            .is_ok()
    );
    assert_eq!(manager.release_device_leases("COM3").unwrap(), 1);
    drop(manager);

    let manager = DeviceManager::with_lease_store(&path).await.unwrap();
    manager.refresh_from_discovered(vec![usb_device("COM3")]);
    assert!(
        manager
            .get_device_status("COM3")
            .unwrap()
            .is_available_for_exclusive()
    );
}

fn usb_device(port: &str) -> DiscoveredDevice {
    DiscoveredDevice {
        port: port.to_string(),
        device_id: "test-usb-device".to_string(),
        description: "Test Device".to_string(),
        vid: None,
        pid: None,
        vendor_name: None,
        product_name: None,
        is_cdc: None,
        serial_number: Some("TEST-SERIAL".to_string()),
        port_health: fbuild_serial::ports::PortHealth::Unknown,
        instance_id: None,
        parent_instance_id: None,
        location_paths: Vec::new(),
    }
}
