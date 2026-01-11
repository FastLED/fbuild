"""
Process Tracking and Cleanup Module

This module manages tracking of build/deploy/monitor processes and their entire
process trees. When client processes die, orphaned process trees are automatically
cleaned up to prevent resource leaks and file locking issues.

Key features:
- Track root process + all children (recursive)
- Detect dead client processes
- Kill entire process trees recursively
- Thread-safe operations for daemon use
"""

import _thread
import json
import logging
import threading
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import psutil


@dataclass
class ProcessTreeInfo:
    """Information about a tracked process tree.

    Attributes:
        client_pid: PID of the client that initiated the operation
        root_pid: PID of the root process
        child_pids: List of all child PIDs (updated periodically)
        request_id: Request ID
        project_dir: Project directory
        operation_type: Type of operation (deploy/monitor)
        port: Serial port (if applicable)
        started_at: Unix timestamp when tracking started
        last_updated: Unix timestamp of last child PID refresh
    """

    client_pid: int
    root_pid: int
    child_pids: list[int] = field(default_factory=list)
    request_id: str = ""
    project_dir: str = ""
    operation_type: str = ""
    port: str | None = None
    started_at: float = field(default_factory=time.time)
    last_updated: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ProcessTreeInfo":
        """Create ProcessTreeInfo from dictionary."""
        return cls(
            client_pid=data["client_pid"],
            root_pid=data["root_pid"],
            child_pids=data.get("child_pids", []),
            request_id=data.get("request_id", ""),
            project_dir=data.get("project_dir", ""),
            operation_type=data.get("operation_type", ""),
            port=data.get("port"),
            started_at=data.get("started_at", time.time()),
            last_updated=data.get("last_updated", time.time()),
        )


class ProcessTracker:
    """Thread-safe tracker for process trees.

    This class maintains a registry of active processes and provides
    methods to detect and cleanup orphaned process trees.
    """

    def __init__(self, registry_file: Path):
        """Initialize the tracker.

        Args:
            registry_file: Path to JSON file for persisting process trees
        """
        self.registry_file = registry_file
        self.lock = threading.Lock()
        self._registry: dict[int, ProcessTreeInfo] = {}
        self._load_registry()
        logging.info(f"ProcessTracker initialized with {len(self._registry)} tracked processes")

    def _load_registry(self) -> None:
        """Load registry from disk (if it exists)."""
        if not self.registry_file.exists():
            return

        try:
            with open(self.registry_file) as f:
                data = json.load(f)

            with self.lock:
                self._registry = {int(client_pid): ProcessTreeInfo.from_dict(info) for client_pid, info in data.items()}

            logging.info(f"Loaded {len(self._registry)} process trees from registry")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Failed to load process registry: {e}")
            self._registry = {}

    def _save_registry(self) -> None:
        """Save registry to disk atomically."""
        try:
            # Prepare data for serialization
            data = {str(client_pid): info.to_dict() for client_pid, info in self._registry.items()}

            # Atomic write
            temp_file = self.registry_file.with_suffix(".tmp")
            with open(temp_file, "w") as f:
                json.dump(data, f, indent=2)

            temp_file.replace(self.registry_file)

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"Failed to save process registry: {e}")

    def register_process(
        self,
        client_pid: int,
        root_pid: int,
        request_id: str = "",
        project_dir: str = "",
        operation_type: str = "",
        port: str | None = None,
    ) -> None:
        """Register a new process tree.

        Args:
            client_pid: PID of client that initiated operation
            root_pid: PID of root process
            request_id: Request ID (optional)
            project_dir: Project directory (optional)
            operation_type: Type of operation (optional)
            port: Serial port (optional)
        """
        with self.lock:
            self._registry[client_pid] = ProcessTreeInfo(
                client_pid=client_pid,
                root_pid=root_pid,
                request_id=request_id,
                project_dir=project_dir,
                operation_type=operation_type,
                port=port,
            )

            # Immediately refresh child PIDs
            self._update_child_pids(client_pid)

        self._save_registry()
        logging.info(f"Registered process tree: client={client_pid}, root={root_pid}, children={len(self._registry[client_pid].child_pids)}, operation={operation_type}")

    def unregister_process(self, client_pid: int) -> None:
        """Remove a process tree from tracking.

        Args:
            client_pid: Client PID to remove
        """
        with self.lock:
            if client_pid in self._registry:
                info = self._registry.pop(client_pid)
                logging.info(f"Unregistered process tree: client={client_pid}, root={info.root_pid}")
            else:
                logging.warning(f"Attempted to unregister unknown client PID: {client_pid}")

        self._save_registry()

    def _update_child_pids(self, client_pid: int) -> None:
        """Update child PID list for a tracked process.

        This method MUST be called with self.lock held.

        Args:
            client_pid: Client PID to update
        """
        if client_pid not in self._registry:
            return

        info = self._registry[client_pid]

        try:
            # Get root process
            root_proc = psutil.Process(info.root_pid)

            # Get ALL descendants recursively
            children = root_proc.children(recursive=True)
            info.child_pids = [child.pid for child in children]
            info.last_updated = time.time()

        except psutil.NoSuchProcess:
            # Root process died - mark as empty
            info.child_pids = []
            info.last_updated = time.time()
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Failed to update child PIDs for client={client_pid}: {e}")

    def refresh_all_child_pids(self) -> None:
        """Refresh child PID lists for all tracked processes."""
        with self.lock:
            for client_pid in list(self._registry.keys()):
                self._update_child_pids(client_pid)

        self._save_registry()

    def cleanup_orphaned_processes(self) -> list[int]:
        """Detect and kill process trees for dead clients.

        Returns:
            List of client PIDs that were cleaned up
        """
        orphaned_clients = []

        with self.lock:
            for client_pid, info in list(self._registry.items()):
                # Check if client is still alive
                if psutil.pid_exists(client_pid):
                    continue

                # Client is dead - kill the entire process tree
                logging.info(f"Client {client_pid} is dead, cleaning up process tree (root={info.root_pid}, children={len(info.child_pids)}, operation={info.operation_type})")

                killed_count = self._kill_process_tree(info)
                orphaned_clients.append(client_pid)

                logging.info(f"Cleaned up {killed_count} processes for dead client {client_pid}")

                # Remove from registry
                del self._registry[client_pid]

        if orphaned_clients:
            logging.info(f"Orphaned clients cleaned up: {orphaned_clients}")
            self._save_registry()

        return orphaned_clients

    def _kill_process_tree(self, info: ProcessTreeInfo) -> int:
        """Kill an entire process tree (root + all children).

        This method MUST be called with self.lock held.

        Args:
            info: ProcessTreeInfo containing root and child PIDs

        Returns:
            Number of processes killed
        """
        killed_count = 0
        all_pids = info.child_pids + [info.root_pid]

        # Refresh child list one last time before killing
        try:
            root_proc = psutil.Process(info.root_pid)
            children = root_proc.children(recursive=True)
            all_pids = [child.pid for child in children] + [info.root_pid]
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            pass  # Use cached PID list

        # Kill children first (bottom-up to avoid orphans)
        processes_to_kill: list[psutil.Process] = []
        for pid in reversed(all_pids):  # Reverse to kill children before parents
            try:
                proc = psutil.Process(pid)
                processes_to_kill.append(proc)
            except psutil.NoSuchProcess:
                pass  # Already dead
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to get process {pid}: {e}")

        logging.info(f"Terminating {len(processes_to_kill)} processes")
        # Terminate all processes
        for proc in processes_to_kill:
            try:
                proc.terminate()
                killed_count += 1
            except psutil.NoSuchProcess:
                pass  # Already dead
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to terminate process {proc.pid}: {e}")

        # Wait for graceful termination
        _gone, alive = psutil.wait_procs(processes_to_kill, timeout=3)

        # Force kill any stragglers
        if alive:
            logging.warning(f"Force killing {len(alive)} stubborn processes")
        for proc in alive:
            try:
                proc.kill()
                logging.warning(f"Force killed stubborn process {proc.pid}")
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to force kill process {proc.pid}: {e}")

        return killed_count

    def get_tracked_clients(self) -> list[int]:
        """Get list of all tracked client PIDs.

        Returns:
            List of client PIDs currently being tracked
        """
        with self.lock:
            return list(self._registry.keys())

    def get_process_info(self, client_pid: int) -> ProcessTreeInfo | None:
        """Get process tree info for a client.

        Args:
            client_pid: Client PID to query

        Returns:
            ProcessTreeInfo if found, None otherwise
        """
        with self.lock:
            return self._registry.get(client_pid)

    def get_processes_by_port(self, port: str) -> list[ProcessTreeInfo]:
        """Get all processes using a specific serial port.

        Args:
            port: Serial port to search for

        Returns:
            List of ProcessTreeInfo for processes using this port
        """
        with self.lock:
            return [info for info in self._registry.values() if info.port == port]

    def get_processes_by_project(self, project_dir: str) -> list[ProcessTreeInfo]:
        """Get all processes for a specific project.

        Args:
            project_dir: Project directory to search for

        Returns:
            List of ProcessTreeInfo for processes in this project
        """
        with self.lock:
            return [info for info in self._registry.values() if info.project_dir == project_dir]
