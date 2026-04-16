"""RED tests: SerialMonitor.reset_device() must exist and return bool.

These tests verify the PyO3 binding exposes reset_device() on SerialMonitor.
They will FAIL until the Rust implementation is added.
"""

from __future__ import annotations

def test_serial_monitor_has_reset_device() -> None:
    """SerialMonitor must expose a reset_device method."""
    from fbuild._native import SerialMonitor

    assert hasattr(SerialMonitor, "reset_device"), (
        "SerialMonitor is missing reset_device method. "
        "Add #[pyo3] fn reset_device(&self, board: Option<String>) -> PyResult<bool> "
        "to the SerialMonitor impl block in crates/fbuild-python/src/lib.rs"
    )


def test_serial_monitor_reset_device_is_callable() -> None:
    """reset_device must be callable (not just an attribute)."""
    from fbuild._native import SerialMonitor

    mon = SerialMonitor(port="COM13", baud_rate=115200)
    assert callable(getattr(mon, "reset_device", None)), (
        "SerialMonitor.reset_device exists but is not callable"
    )
