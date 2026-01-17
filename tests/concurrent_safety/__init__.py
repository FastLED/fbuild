"""
Concurrent safety tests for fbuild daemon operations.

This package contains tests that verify concurrent safety of ESP32-C6
builds with deployment and monitoring. Tests cover:
1. Lock contention and acquisition
2. Same-project build conflicts
3. Same-port deploy conflicts
4. Lock persistence through deploy+monitor cycles
5. COM port state tracking
6. End-to-end single device integration

Test markers:
- concurrent: All concurrent safety tests
- hardware: Tests that require ESP32 hardware
- single_device: Tests that require exactly 1 ESP32-C6 device
"""
