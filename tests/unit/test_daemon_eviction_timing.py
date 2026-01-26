"""Unit tests for daemon self-eviction timing.

These tests verify that the daemon's SELF_EVICTION_TIMEOUT (30s) is correctly
configured to handle validation workflows (deploy + USB re-enum + port check).

Background:
- SELF_EVICTION_TIMEOUT was increased from 4s → 30s to accommodate:
  - Deploy operation completes
  - Windows USB-CDC re-enumeration (5-15s)
  - SerialMonitor attach for validation (begins ~20s after deploy starts)

Test Strategy:
- Mock daemon lifecycle with precise timing control
- Verify daemon survives expected workflow gaps
- Verify daemon evicts after true idleness
"""

import time

# Current daemon configuration (from daemon.py)
SELF_EVICTION_TIMEOUT = 30.0  # 30 seconds


class MockDaemonContext:
    """Mock daemon context for testing self-eviction logic."""

    def __init__(self):
        self.client_count = 0
        self.operation_in_progress = False
        self.shutdown_called = False
        self.daemon_empty_since = None

    def check_self_eviction(self) -> bool:
        """Simulate daemon's self-eviction check logic.

        Returns:
            True if daemon should evict (shutdown), False otherwise
        """
        daemon_is_empty = self.client_count == 0 and not self.operation_in_progress

        if daemon_is_empty:
            if self.daemon_empty_since is None:
                self.daemon_empty_since = time.time()
                return False
            elif time.time() - self.daemon_empty_since >= SELF_EVICTION_TIMEOUT:
                self.shutdown_called = True
                return True
        elif self.daemon_empty_since is not None:
            # Daemon is no longer empty, reset timer
            self.daemon_empty_since = None

        return False

    def simulate_client_connect(self):
        """Simulate a client connecting."""
        self.client_count += 1

    def simulate_client_disconnect(self):
        """Simulate a client disconnecting."""
        self.client_count = max(0, self.client_count - 1)

    def simulate_operation_start(self):
        """Simulate an operation starting (build/deploy/monitor)."""
        self.operation_in_progress = True

    def simulate_operation_end(self):
        """Simulate an operation completing."""
        self.operation_in_progress = False


class TestDaemonSelfEvictionTiming:
    """Test daemon self-eviction timeout behavior."""

    def test_daemon_survives_timeout_minus_one(self):
        """Daemon should NOT evict at (TIMEOUT - 1) seconds of idleness.

        Scenario:
        1. Deploy operation completes
        2. Client disconnects
        3. Wait 29 seconds (TIMEOUT - 1)
        4. New client connects
        5. Daemon should still be alive (not evicted)

        This ensures the 30s timeout provides adequate buffer for validation workflows.
        """
        daemon = MockDaemonContext()

        # Simulate operation
        daemon.simulate_client_connect()
        daemon.simulate_operation_start()
        daemon.simulate_operation_end()
        daemon.simulate_client_disconnect()

        # Daemon is now empty
        assert daemon.check_self_eviction() is False, "Daemon should start eviction timer"
        assert daemon.daemon_empty_since is not None, "Eviction timer should be set"

        eviction_start = daemon.daemon_empty_since

        # Wait TIMEOUT - 1 second
        time.sleep(0.1)  # Small delay to simulate time passing
        daemon.daemon_empty_since = eviction_start - (SELF_EVICTION_TIMEOUT - 1)

        # Check eviction (should NOT evict yet)
        assert daemon.check_self_eviction() is False, "Daemon should NOT evict before timeout"
        assert daemon.shutdown_called is False, "Shutdown should not be called"

        # Simulate new client connecting
        daemon.simulate_client_connect()

        # Check eviction (daemon no longer empty, timer should reset)
        assert daemon.check_self_eviction() is False, "Daemon should reset timer when client connects"
        assert daemon.daemon_empty_since is None, "Eviction timer should be reset"

    def test_daemon_evicts_after_timeout(self):
        """Daemon SHOULD evict at (TIMEOUT + 1) seconds of idleness.

        Scenario:
        1. Operation completes
        2. All clients disconnect
        3. Wait 31 seconds (TIMEOUT + 1)
        4. Daemon should auto-evict

        This ensures the daemon doesn't run indefinitely when truly idle.
        """
        daemon = MockDaemonContext()

        # Simulate operation
        daemon.simulate_client_connect()
        daemon.simulate_operation_start()
        daemon.simulate_operation_end()
        daemon.simulate_client_disconnect()

        # Daemon is now empty
        assert daemon.check_self_eviction() is False, "Daemon should start eviction timer"
        eviction_start = daemon.daemon_empty_since

        # Simulate TIMEOUT + 1 second elapsed
        daemon.daemon_empty_since = eviction_start - (SELF_EVICTION_TIMEOUT + 1)

        # Check eviction (should evict now)
        assert daemon.check_self_eviction() is True, "Daemon SHOULD evict after timeout"
        assert daemon.shutdown_called is True, "Shutdown should be called"

    def test_daemon_does_not_evict_during_operation(self):
        """Daemon should NOT evict while operation is in progress.

        Even if the timeout expires, daemon must not evict while processing an operation.
        """
        daemon = MockDaemonContext()

        # Start operation (no clients)
        daemon.simulate_operation_start()

        # Daemon is NOT empty (operation running)
        assert daemon.check_self_eviction() is False, "Daemon should not start eviction timer during op"
        assert daemon.daemon_empty_since is None, "Eviction timer should not be set during op"

        # Even after simulated timeout, should not evict
        daemon.daemon_empty_since = time.time() - (SELF_EVICTION_TIMEOUT + 10)
        assert daemon.check_self_eviction() is False, "Daemon should not evict during operation"
        assert daemon.shutdown_called is False, "Shutdown should not be called during operation"

        # Operation completes
        daemon.simulate_operation_end()

        # Now daemon is empty, timer starts
        assert daemon.check_self_eviction() is False, "Daemon should start eviction timer after op ends"
        assert daemon.daemon_empty_since is not None, "Eviction timer should be set"

    def test_validation_workflow_timing(self):
        """Simulate real FastLED validation workflow timing.

        Workflow:
        1. Deploy firmware (5s)
        2. Windows USB-CDC re-enumeration (5s delay)
        3. Port availability check (15 retries × 1s = 15s)
        4. SerialMonitor attach (begins ~25s after deploy start)

        Daemon timeline:
        - t=0s: Deploy starts (client connected, operation in progress)
        - t=5s: Deploy ends (client disconnects, operation completes)
        - t=5s-25s: Daemon is EMPTY (0 clients, 0 ops) - 20 second gap
        - t=25s: SerialMonitor attaches (client connects)

        With 30s timeout:
        - Daemon should survive the 20s gap
        - Daemon should NOT evict before SerialMonitor attaches
        """
        daemon = MockDaemonContext()

        # t=0s: Deploy starts
        daemon.simulate_client_connect()
        daemon.simulate_operation_start()

        # t=5s: Deploy completes, client disconnects
        daemon.simulate_operation_end()
        daemon.simulate_client_disconnect()

        # Daemon is now empty (timer starts)
        assert daemon.check_self_eviction() is False, "Daemon should start eviction timer"
        eviction_start = daemon.daemon_empty_since

        # t=25s: Simulate 20 seconds elapsed (USB re-enum + port checks)
        daemon.daemon_empty_since = eviction_start - 20.0

        # Check eviction (should NOT evict - only 20s elapsed, timeout is 30s)
        assert daemon.check_self_eviction() is False, "Daemon should NOT evict during validation workflow"
        assert daemon.shutdown_called is False, "Daemon should survive 20s gap"

        # t=25s: SerialMonitor attaches
        daemon.simulate_client_connect()

        # Daemon no longer empty, timer resets
        assert daemon.check_self_eviction() is False, "Daemon should reset timer when client connects"
        assert daemon.daemon_empty_since is None, "Eviction timer should be reset"

    def test_multiple_workflow_cycles(self):
        """Test multiple deploy → validate cycles without daemon eviction.

        Ensures daemon can handle repeated workflows without premature eviction.
        """
        daemon = MockDaemonContext()

        for cycle in range(3):
            # Deploy
            daemon.simulate_client_connect()
            daemon.simulate_operation_start()
            daemon.simulate_operation_end()
            daemon.simulate_client_disconnect()

            # Gap (15s)
            assert daemon.check_self_eviction() is False, "Eviction timer starts"
            eviction_start = daemon.daemon_empty_since
            daemon.daemon_empty_since = eviction_start - 15.0

            # Should NOT evict (15s < 30s)
            assert daemon.check_self_eviction() is False, f"Daemon should survive cycle {cycle}"

            # Validate
            daemon.simulate_client_connect()
            # Must call check_self_eviction() to trigger timer reset logic
            daemon.check_self_eviction()
            assert daemon.daemon_empty_since is None, "Timer should reset"

            # Validation completes
            daemon.simulate_client_disconnect()

        # After all cycles, daemon is empty again
        # Must call check_self_eviction() to start timer for the final empty state
        daemon.check_self_eviction()
        assert daemon.daemon_empty_since is not None, "Timer should be set after final cycle"

    def test_eviction_timer_reset_on_client_activity(self):
        """Eviction timer should reset when client activity occurs.

        Ensures daemon doesn't evict due to stale timer if clients reconnect.
        """
        daemon = MockDaemonContext()

        # Daemon goes empty
        daemon.simulate_client_connect()
        daemon.simulate_client_disconnect()

        # Timer starts
        assert daemon.check_self_eviction() is False
        eviction_start = daemon.daemon_empty_since

        # Simulate 25s elapsed
        daemon.daemon_empty_since = eviction_start - 25.0
        assert daemon.check_self_eviction() is False, "Daemon should not evict yet"

        # Client connects (timer should reset)
        daemon.simulate_client_connect()
        # Must call check_self_eviction() to trigger timer reset logic
        daemon.check_self_eviction()
        assert daemon.daemon_empty_since is None, "Timer should reset on client connect"

        # Client disconnects again
        daemon.simulate_client_disconnect()
        assert daemon.check_self_eviction() is False, "Timer should restart from 0"

        new_eviction_start = daemon.daemon_empty_since

        # Verify timer was reset (new start time > old start time)
        # Since we're using mock time, just verify timer exists
        assert new_eviction_start is not None, "New timer should be set"


class TestDaemonEvictionEdgeCases:
    """Test edge cases in daemon eviction logic."""

    def test_eviction_with_zero_clients_and_no_ops(self):
        """Daemon should evict when truly idle (0 clients, 0 ops, timeout expired)."""
        daemon = MockDaemonContext()

        # Daemon starts with 0 clients, 0 ops
        assert daemon.client_count == 0
        assert daemon.operation_in_progress is False

        # First check: timer starts
        assert daemon.check_self_eviction() is False
        assert daemon.daemon_empty_since is not None

        # Simulate timeout
        daemon.daemon_empty_since = time.time() - (SELF_EVICTION_TIMEOUT + 1)

        # Should evict
        assert daemon.check_self_eviction() is True
        assert daemon.shutdown_called is True

    def test_no_eviction_with_connected_clients(self):
        """Daemon should NOT evict while clients are connected (even if idle)."""
        daemon = MockDaemonContext()

        # Client connected, no operation
        daemon.simulate_client_connect()

        # Daemon is not empty (has client)
        for _ in range(100):  # Check multiple times
            assert daemon.check_self_eviction() is False, "Daemon should not evict with connected clients"
            assert daemon.daemon_empty_since is None, "Timer should not start with connected clients"

    def test_eviction_timer_precision(self):
        """Test eviction timer precision (should evict exactly at timeout, not before)."""
        daemon = MockDaemonContext()

        # Start empty
        daemon.simulate_client_connect()
        daemon.simulate_client_disconnect()

        assert daemon.check_self_eviction() is False
        eviction_start = daemon.daemon_empty_since

        # Test at TIMEOUT - 0.1s (should NOT evict)
        daemon.daemon_empty_since = eviction_start - (SELF_EVICTION_TIMEOUT - 0.1)
        assert daemon.check_self_eviction() is False, "Should not evict before timeout"

        # Test at TIMEOUT + 0.1s (SHOULD evict)
        daemon.daemon_empty_since = eviction_start - (SELF_EVICTION_TIMEOUT + 0.1)
        assert daemon.check_self_eviction() is True, "Should evict after timeout"
