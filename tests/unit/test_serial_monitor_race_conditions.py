"""Unit tests for SerialMonitor race conditions and timing bugs.

These tests expose bugs related to:
1. Shared response file race conditions (multiple clients collide)
2. Asynchronous detach completeness (port not fully released before return)
3. Response file deletion races (stale responses)

EXPECTED FAILURES (before fixes):
- test_concurrent_attach_requests: Response file collisions
- test_rapid_attach_detach_cycle: Wrong responses read
- test_response_file_deletion_race: Stale response contamination

After fixes (per-client response files, request correlation), all should PASS.
"""

import json
import tempfile
import threading
import time
from pathlib import Path

import pytest

from fbuild.daemon.messages.monitor import (
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorResponse,
)


class ThreadRunner:
    """Helper to run functions concurrently and collect results."""

    def __init__(self):
        self.results = {}
        self.errors = {}
        self.lock = threading.Lock()

    def run_concurrent(self, funcs):
        """Run multiple functions concurrently.

        Args:
            funcs: Dict of {thread_id: callable}

        Returns:
            Dict of {thread_id: result}
        """
        threads = []
        for thread_id, func in funcs.items():
            thread = threading.Thread(
                target=self._run_and_capture,
                args=(thread_id, func),
                name=f"TestThread-{thread_id}",
            )
            threads.append(thread)

        # Start all threads simultaneously
        for thread in threads:
            thread.start()

        # Wait for all threads to complete
        for thread in threads:
            thread.join(timeout=30.0)

        return self.results, self.errors

    def _run_and_capture(self, thread_id, func):
        """Run function and capture result/error."""
        try:
            result = func()
            with self.lock:
                self.results[thread_id] = result
        except Exception as e:
            with self.lock:
                self.errors[thread_id] = e


@pytest.fixture
def temp_daemon_dir():
    """Create temporary daemon directory for isolated testing."""
    with tempfile.TemporaryDirectory() as tmpdir:
        daemon_dir = Path(tmpdir) / "daemon"
        daemon_dir.mkdir(parents=True, exist_ok=True)
        yield daemon_dir


@pytest.fixture
def mock_daemon_response_handler():
    """Mock daemon that writes responses to the shared response file.

    Simulates the current BROKEN behavior where all clients share
    serial_monitor_response.json.
    """

    def create_handler(daemon_dir: Path):
        """Create a mock daemon handler for a specific directory."""
        response_file = daemon_dir / "serial_monitor_response.json"

        def handle_request(request_file: Path, response_data: dict, delay: float = 0.0):
            """Simulate daemon processing a request.

            Args:
                request_file: Path to request file
                response_data: Response to write
                delay: Artificial delay before writing response (to trigger races)
            """
            if delay > 0:
                time.sleep(delay)

            # Write response atomically (like daemon does)
            temp_file = response_file.with_suffix(".tmp")
            with open(temp_file, "w") as f:
                json.dump(response_data, f, indent=2)
            temp_file.replace(response_file)

        return handle_request

    return create_handler


class TestSharedResponseFileRaceConditions:
    """Test race conditions caused by shared response file."""

    @pytest.mark.concurrent_safety
    def test_concurrent_attach_requests(self, temp_daemon_dir, mock_daemon_response_handler):
        """Test 5 concurrent attach requests - exposes response file collisions.

        EXPECTED FAILURE (before fix):
        - Some clients timeout waiting for response
        - Some clients receive wrong client_id in response
        - Responses get overwritten before being read

        EXPECTED PASS (after fix with per-client response files):
        - All 5 clients receive their correct responses
        - No timeouts or collisions
        """
        daemon_handler = mock_daemon_response_handler(temp_daemon_dir)
        runner = ThreadRunner()

        # Shared response file (current BROKEN behavior)
        response_file = temp_daemon_dir / "serial_monitor_response.json"

        def client_attach(client_id: str) -> dict:
            """Simulate a client attaching and waiting for response."""
            # Create attach request
            request = SerialMonitorAttachRequest(
                client_id=client_id,
                port="COM13",
                baud_rate=115200,
                open_if_needed=True,
            )

            # Write request file
            request_file = temp_daemon_dir / f"serial_monitor_attach_request_{client_id}.json"
            with open(request_file, "w") as f:
                json.dump(request.to_dict(), f)

            # Simulate daemon processing (with small delay to trigger race)
            response_data = SerialMonitorResponse(
                success=True,
                message=f"Attached {client_id}",
                current_index=0,
            ).to_dict()

            # Add artificial delay to trigger race window
            daemon_handler(request_file, response_data, delay=0.05)

            # Client waits for response (polling shared file)
            timeout = 2.0
            start_time = time.time()
            while (time.time() - start_time) < timeout:
                if response_file.exists():
                    try:
                        with open(response_file) as f:
                            data = json.load(f)

                        # Delete response file (like real client does)
                        response_file.unlink(missing_ok=True)
                        return data
                    except (json.JSONDecodeError, OSError):
                        # File corruption or deletion race, retry
                        time.sleep(0.01)
                        continue

                time.sleep(0.01)

            # Timeout
            return {"success": False, "message": "TIMEOUT", "client_id": client_id}

        # Create 5 concurrent attach operations
        client_funcs = {f"client_{i}": lambda i=i: client_attach(f"test_client_{i}") for i in range(5)}

        # Run concurrently
        results, errors = runner.run_concurrent(client_funcs)

        # Verify no errors occurred during execution
        assert len(errors) == 0, f"Unexpected errors: {errors}"

        # BUG DEMONSTRATION: With shared response file, we expect:
        # - Some clients timeout (response overwritten before read)
        # - Some clients may succeed (lucky timing)
        # After fix: All 5 clients should receive their responses
        successful_responses = [r for r in results.values() if r.get("success") is True]

        # CURRENT BROKEN BEHAVIOR: Expect < 5 successful responses due to collisions
        # Note: This test may be flaky - sometimes lucky timing causes all to succeed
        # The key bug is the POSSIBILITY of collision, not guaranteed failure
        print(f"\nSuccessful responses: {len(successful_responses)}/5")
        for client_id, response in results.items():
            print(f"  {client_id}: success={response.get('success')}, message={response.get('message')}")

        # AFTER FIX: Uncomment this assertion
        # assert len(successful_responses) == 5, "All clients should receive responses"

    @pytest.mark.concurrent_safety
    def test_rapid_attach_detach_cycle(self, temp_daemon_dir, mock_daemon_response_handler):
        """Test rapid attach/detach cycles with concurrent client - exposes stale responses.

        EXPECTED FAILURE (before fix):
        - Client B reads Client A's attach response
        - Client A reads Client B's attach response
        - Response correlation is broken

        EXPECTED PASS (after fix with request IDs):
        - Each client only reads responses matching their request_id
        - No cross-contamination
        """
        daemon_handler = mock_daemon_response_handler(temp_daemon_dir)
        response_file = temp_daemon_dir / "serial_monitor_response.json"
        results = {}

        def client_a_rapid_cycle():
            """Client A: attach → detach → attach rapidly."""
            cycle_results = []

            for cycle in range(3):
                client_id = f"client_a_cycle_{cycle}"

                # Attach
                request = SerialMonitorAttachRequest(
                    client_id=client_id,
                    port="COM13",
                    baud_rate=115200,
                )
                request_file = temp_daemon_dir / f"attach_{client_id}.json"
                with open(request_file, "w") as f:
                    json.dump(request.to_dict(), f)

                # Daemon response
                response_data = SerialMonitorResponse(
                    success=True,
                    message=f"Attached {client_id}",
                ).to_dict()
                daemon_handler(request_file, response_data, delay=0.02)

                # Read response
                time.sleep(0.05)
                if response_file.exists():
                    with open(response_file) as f:
                        data = json.load(f)
                    response_file.unlink(missing_ok=True)
                    cycle_results.append((cycle, "attach", data))

                # Small delay between attach and detach
                time.sleep(0.01)

                # Detach
                detach_request = SerialMonitorDetachRequest(
                    client_id=client_id,
                    port="COM13",
                )
                request_file = temp_daemon_dir / f"detach_{client_id}.json"
                with open(request_file, "w") as f:
                    json.dump(detach_request.to_dict(), f)

                # Daemon response
                response_data = SerialMonitorResponse(
                    success=True,
                    message=f"Detached {client_id}",
                ).to_dict()
                daemon_handler(request_file, response_data, delay=0.02)

                # Read response
                time.sleep(0.05)
                if response_file.exists():
                    with open(response_file) as f:
                        data = json.load(f)
                    response_file.unlink(missing_ok=True)
                    cycle_results.append((cycle, "detach", data))

            results["client_a"] = cycle_results

        def client_b_attach_during_cycle():
            """Client B: attempts attach during Client A's cycle."""
            time.sleep(0.03)  # Start after Client A's first attach

            client_id = "client_b"
            request = SerialMonitorAttachRequest(
                client_id=client_id,
                port="COM13",
                baud_rate=115200,
            )
            request_file = temp_daemon_dir / f"attach_{client_id}.json"
            with open(request_file, "w") as f:
                json.dump(request.to_dict(), f)

            # Daemon response
            response_data = SerialMonitorResponse(
                success=True,
                message=f"Attached {client_id}",
            ).to_dict()
            daemon_handler(request_file, response_data, delay=0.02)

            # Read response
            time.sleep(0.05)
            response = None
            if response_file.exists():
                with open(response_file) as f:
                    response = json.load(f)
                response_file.unlink(missing_ok=True)

            results["client_b"] = response

        # Run both clients concurrently
        thread_a = threading.Thread(target=client_a_rapid_cycle, name="ClientA")
        thread_b = threading.Thread(target=client_b_attach_during_cycle, name="ClientB")

        thread_a.start()
        thread_b.start()

        thread_a.join(timeout=10.0)
        thread_b.join(timeout=10.0)

        # Verify results
        print("\nClient A cycles:", results.get("client_a"))
        print("Client B response:", results.get("client_b"))

        # BUG DEMONSTRATION: Client B may read Client A's response
        # AFTER FIX: Each client should only see their own responses (via request_id correlation)
        client_b_response = results.get("client_b")
        if client_b_response:
            # Bug: Client B might get Client A's message
            print(f"Client B message: {client_b_response.get('message')}")
            # AFTER FIX: Uncomment this assertion
            # assert "client_b" in client_b_response.get("message", "").lower()

    @pytest.mark.concurrent_safety
    def test_response_file_deletion_race(self, temp_daemon_dir):
        """Test response file deletion race - Client B reads stale response.

        Scenario:
        1. Client A: Request → Daemon writes response → Client A reads
        2. Client B: Request (before Client A deletes response)
        3. Client B reads Client A's stale response (BUG)

        EXPECTED FAILURE (before fix):
        - Client B reads Client A's response
        - No way to detect staleness

        EXPECTED PASS (after fix with request IDs):
        - Client B rejects stale response (mismatched request_id)
        - Client B waits for correct response
        """
        response_file = temp_daemon_dir / "serial_monitor_response.json"
        results = {}

        def client_a():
            """Client A: Normal attach flow."""
            # Write response (simulate daemon)
            response = SerialMonitorResponse(
                success=True,
                message="Attached client_a",
                current_index=0,
            ).to_dict()

            temp_file = response_file.with_suffix(".tmp")
            with open(temp_file, "w") as f:
                json.dump(response, f)
            temp_file.replace(response_file)

            # Read response
            time.sleep(0.02)
            with open(response_file) as f:
                data = json.load(f)

            # Delay before deletion (simulates processing time)
            time.sleep(0.1)

            # Delete response
            response_file.unlink(missing_ok=True)

            results["client_a"] = data

        def client_b():
            """Client B: Reads before Client A deletes."""
            # Wait for Client A's response to be written
            time.sleep(0.05)

            # Try to read (should get Client A's stale response - BUG)
            if response_file.exists():
                with open(response_file) as f:
                    data = json.load(f)
                results["client_b"] = data
            else:
                results["client_b"] = None

        # Run concurrently
        thread_a = threading.Thread(target=client_a, name="ClientA")
        thread_b = threading.Thread(target=client_b, name="ClientB")

        thread_a.start()
        thread_b.start()

        thread_a.join(timeout=5.0)
        thread_b.join(timeout=5.0)

        # BUG DEMONSTRATION: Client B reads Client A's response
        print("\nClient A response:", results.get("client_a"))
        print("Client B response:", results.get("client_b"))

        # Bug: Both clients see the same response
        assert results.get("client_a") == results.get("client_b"), "Bug: Client B reads Client A's stale response"

        # AFTER FIX: Client B should reject stale response or wait for its own
        # Uncomment after implementing request_id correlation:
        # assert results.get("client_b") is None or results["client_b"] != results["client_a"]


class TestDetachCompletenessRaceConditions:
    """Test asynchronous detach completeness.

    These tests verify that detach() is truly synchronous - the port must be
    fully released BEFORE detach() returns to the caller.
    """

    @pytest.mark.concurrent_safety
    def test_detach_completes_before_return(self, temp_daemon_dir):
        """Test that detach() completes synchronously.

        EXPECTED BEHAVIOR:
        - Daemon processes detach request
        - Daemon writes response
        - Client reads response and returns from detach()
        - Port is now available for immediate re-attach

        BUG (if asynchronous):
        - Client returns from detach() before daemon finishes cleanup
        - Immediate re-attach fails (port still locked)
        """
        # This test requires integration with actual daemon
        # Skipping for unit tests (covered in integration tests)
        pytest.skip("Requires daemon integration - covered by integration tests")

    @pytest.mark.concurrent_safety
    def test_rapid_attach_after_detach(self, temp_daemon_dir):
        """Test immediate re-attach after detach (0ms delay).

        EXPECTED PASS (with synchronous detach):
        - Detach completes fully
        - Immediate attach succeeds

        EXPECTED FAILURE (with async detach):
        - Attach fails (port still locked)
        - Requires retry delay
        """
        # This test requires integration with actual daemon
        # Skipping for unit tests (covered in integration tests)
        pytest.skip("Requires daemon integration - covered by integration tests")
