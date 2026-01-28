"""Test Suite 5: Windows Kernel Lock Detection

This test suite provides DIAGNOSTIC capabilities to detect and distinguish
between kernel-level and process-level serial port locks on Windows.

**Purpose:** Diagnostic tool, not a functional test
**Platform:** Windows only (uses ctypes to call Windows API)
**Use Case:** Help identify WHY a port is locked during debugging

Root Cause Analysis Context:
- Issue #6 from SUMMARY.md: Port locking investigation
- Windows kernel can hold port locks at driver level
- Process locks are different from kernel locks
- This test helps distinguish between the two

Test Strategy:
1. Use CreateFileW Windows API to attempt port access
2. Parse error codes to identify lock type
3. Provide diagnostic information for debugging
4. Does NOT fix locks - only identifies them
"""

import ctypes
import sys
from ctypes import wintypes

import pytest


@pytest.mark.diagnostic
@pytest.mark.esp32s3
@pytest.mark.skipif(sys.platform != "win32", reason="Windows-specific diagnostic test")
def test_detect_kernel_lock(esp32s3_port):
    """Diagnostic test to detect kernel-level vs process-level port locks.

    This test uses Windows CreateFileW API to detect port lock types.
    It is NOT a functional test - it's a diagnostic tool.

    Test Flow:
    1. Verify port is accessible via pyserial (baseline)
    2. Use ctypes to call CreateFileW on port
    3. Parse error codes if open fails
    4. Distinguish between lock types
    5. Report diagnostic information

    Expected Outcomes:
    - If port accessible: Test documents Windows API behavior
    - If port locked: Test identifies lock type (kernel vs process)

    Root Cause Context:
    - Issue #6: Port locking investigation
    - Windows kernel locks are different from process locks
    - Kernel lock: ERROR_ACCESS_DENIED (error code 5)
    - Process lock: Other error codes (e.g., sharing violation)

    Args:
        esp32s3_port: Port fixture (e.g., "COM13")
    """
    import serial

    # Windows API constants
    GENERIC_READ = 0x80000000
    OPEN_EXISTING = 3
    INVALID_HANDLE_VALUE = -1
    ERROR_ACCESS_DENIED = 5
    ERROR_SHARING_VIOLATION = 32

    def is_kernel_locked(port: str) -> tuple[bool, int, str]:
        """Attempt to open port using Windows CreateFileW API.

        Args:
            port: Port name (e.g., "COM13")

        Returns:
            Tuple of (is_locked, error_code, diagnosis)
            - is_locked: True if port cannot be opened
            - error_code: Windows error code (0 if opened successfully)
            - diagnosis: Human-readable diagnostic message
        """
        # Convert port name to Windows device path
        device_path = f"\\\\.\\{port}"

        # Attempt to open port with GENERIC_READ access
        handle = ctypes.windll.kernel32.CreateFileW(
            device_path,
            GENERIC_READ,  # Desired access
            0,  # Share mode (no sharing)
            None,  # Security attributes
            OPEN_EXISTING,  # Creation disposition
            0,  # Flags and attributes
            None,  # Template file
        )

        if handle == INVALID_HANDLE_VALUE:
            # Get error code
            error = ctypes.windll.kernel32.GetLastError()

            # Diagnose lock type based on error code
            if error == ERROR_ACCESS_DENIED:
                diagnosis = "Kernel-level lock detected (ERROR_ACCESS_DENIED)"
                return True, error, diagnosis
            elif error == ERROR_SHARING_VIOLATION:
                diagnosis = "Process-level lock detected (ERROR_SHARING_VIOLATION)"
                return True, error, diagnosis
            else:
                diagnosis = f"Port locked with unknown error code {error}"
                return True, error, diagnosis
        else:
            # Success - close handle and return
            try:
                ctypes.windll.kernel32.CloseHandle(handle)
                diagnosis = "Port accessible (no lock detected)"
                return False, 0, diagnosis
            except Exception as e:
                diagnosis = f"Port opened but cleanup failed: {e}"
                return False, 0, diagnosis

    # Step 1: Baseline check using pyserial
    print(f"\n[Diagnostic] Testing port: {esp32s3_port}")

    pyserial_accessible = False
    pyserial_error = None

    try:
        with serial.Serial(esp32s3_port, 115200, timeout=1) as ser:
            pyserial_accessible = True
            print(f"[Diagnostic] ✅ Pyserial: Port accessible")
    except serial.SerialException as e:
        pyserial_error = str(e)
        print(f"[Diagnostic] ❌ Pyserial: Port NOT accessible - {e}")
    except Exception as e:
        pyserial_error = str(e)
        print(f"[Diagnostic] ❌ Pyserial: Unexpected error - {e}")

    # Step 2: Windows API check
    is_locked, error_code, diagnosis = is_kernel_locked(esp32s3_port)

    print(f"[Diagnostic] Windows API Result:")
    print(f"  - Locked: {is_locked}")
    print(f"  - Error Code: {error_code}")
    print(f"  - Diagnosis: {diagnosis}")

    # Step 3: Cross-reference results
    print(f"\n[Diagnostic] Cross-Reference Analysis:")

    if pyserial_accessible and not is_locked:
        print("  ✅ CONSISTENT: Both methods report port accessible")
        print("  → Port is healthy and ready for use")
    elif not pyserial_accessible and is_locked:
        print("  ✅ CONSISTENT: Both methods report port locked")
        print(f"  → Lock Type: {diagnosis}")
        print(f"  → Pyserial Error: {pyserial_error}")

        if error_code == ERROR_ACCESS_DENIED:
            print("  → RECOVERY: Kernel lock requires USB disconnect/reconnect")
        elif error_code == ERROR_SHARING_VIOLATION:
            print("  → RECOVERY: Process lock - kill process or wait for release")
    elif pyserial_accessible and is_locked:
        print("  ⚠️ INCONSISTENT: Pyserial success but Windows API reports lock")
        print("  → Possible cause: Pyserial bypasses some lock checks")
        print("  → Port may be partially accessible")
    else:  # not pyserial_accessible and not is_locked
        print("  ⚠️ INCONSISTENT: Pyserial failed but Windows API reports accessible")
        print("  → Possible cause: Pyserial-specific issue (baudrate, parity, etc.)")
        print("  → Port is likely accessible via other methods")

    # Step 4: Diagnostic recommendations
    print(f"\n[Diagnostic] Recommendations:")

    if pyserial_accessible:
        print("  ✅ Port is accessible - tests can proceed")
    else:
        print("  ❌ Port is NOT accessible - investigation required")

        if is_locked and error_code == ERROR_ACCESS_DENIED:
            print("  → Action: Disconnect/reconnect USB cable")
            print("  → Root Cause: Windows driver-level lock")
        elif is_locked and error_code == ERROR_SHARING_VIOLATION:
            print("  → Action: Identify and close process holding port")
            print("  → Root Cause: Another process has exclusive access")
        else:
            print("  → Action: Check device manager, drivers, and cables")
            print("  → Root Cause: Unknown - requires deeper investigation")

    # Step 5: Test assertion (diagnostic mode)
    # This test ALWAYS passes - it's a diagnostic tool, not a functional test
    # The output above provides the actual diagnostic information
    assert True, "Diagnostic test completed (see output above for results)"


# Manual debugging helper (not a pytest test)
def _manual_diagnostic(port: str = "COM13"):
    """Standalone diagnostic function for manual debugging.

    This can be run directly from Python interpreter:
    >>> from test_kernel_lock_detection import _manual_diagnostic
    >>> _manual_diagnostic("COM13")

    Args:
        port: Port name to diagnose
    """
    if sys.platform != "win32":
        print("ERROR: This diagnostic is Windows-only")
        return

    print(f"=== Manual Port Diagnostic ===")
    print(f"Port: {port}")
    print()

    # Run the diagnostic
    class FakeFixture:
        """Minimal fixture replacement for manual testing."""

        pass

    fixture = FakeFixture()
    test_detect_kernel_lock(port)
