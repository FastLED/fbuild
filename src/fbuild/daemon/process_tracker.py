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
        logging.debug(f"Initializing ProcessTracker with registry file: {registry_file}")
        self.registry_file = registry_file
        self.lock = threading.Lock()
        self._registry: dict[int, ProcessTreeInfo] = {}
        logging.debug("Loading existing process registry from disk")
        self._load_registry()
        logging.info(f"ProcessTracker initialized with {len(self._registry)} tracked processes")

    def _load_registry(self) -> None:
        """Load registry from disk (if it exists)."""
        logging.debug(f"Checking if registry file exists: {self.registry_file}")
        if not self.registry_file.exists():
            logging.debug("Registry file does not exist, starting with empty registry")
            return

        try:
            logging.debug(f"Reading registry file: {self.registry_file}")
            with open(self.registry_file) as f:
                data = json.load(f)

            logging.debug(f"Parsing {len(data)} registry entries from JSON")
            with self.lock:
                self._registry = {int(client_pid): ProcessTreeInfo.from_dict(info) for client_pid, info in data.items()}

            logging.info(f"Loaded {len(self._registry)} process trees from registry")
            logging.debug(f"Registry entries: {list(self._registry.keys())}")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Failed to load process registry: {e}")
            logging.debug("Initializing empty registry due to load failure")
            self._registry = {}

    def _save_registry(self) -> None:
        """Save registry to disk atomically."""
        try:
            logging.debug(f"Saving process registry to: {self.registry_file}")
            # Prepare data for serialization
            data = {str(client_pid): info.to_dict() for client_pid, info in self._registry.items()}
            logging.debug(f"Serializing {len(data)} registry entries to JSON")

            # Atomic write
            temp_file = self.registry_file.with_suffix(".tmp")
            logging.debug(f"Writing to temporary file: {temp_file}")
            with open(temp_file, "w") as f:
                json.dump(data, f, indent=2)

            logging.debug("Atomically replacing registry file")
            temp_file.replace(self.registry_file)
            logging.debug(f"Registry saved successfully with {len(data)} entries")

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
        logging.info(f"Registering process tree: client_pid={client_pid}, root_pid={root_pid}, operation={operation_type}")
        logging.debug(f"Process details: project_dir={project_dir}, port={port}, request_id={request_id}")

        with self.lock:
            logging.debug(f"Creating ProcessTreeInfo entry for client {client_pid}")
            self._registry[client_pid] = ProcessTreeInfo(
                client_pid=client_pid,
                root_pid=root_pid,
                request_id=request_id,
                project_dir=project_dir,
                operation_type=operation_type,
                port=port,
            )

            logging.debug(f"Building process tree for root PID {root_pid}")
            # Immediately refresh child PIDs
            self._update_child_pids(client_pid)

        logging.debug(f"Persisting registry to disk with {len(self._registry)} entries")
        self._save_registry()
        logging.info(f"Registered process tree: client={client_pid}, root={root_pid}, children={len(self._registry[client_pid].child_pids)}, operation={operation_type}")
        logging.debug(f"Total tracked processes: {len(self._registry)}")

    def unregister_process(self, client_pid: int) -> None:
        """Remove a process tree from tracking.

        Args:
            client_pid: Client PID to remove
        """
        logging.debug(f"Unregistering process tree for client {client_pid}")
        with self.lock:
            if client_pid in self._registry:
                info = self._registry.pop(client_pid)
                logging.info(f"Unregistered process tree: client={client_pid}, root={info.root_pid}")
                logging.debug(f"Removed process tree with {len(info.child_pids)} children")
            else:
                logging.warning(f"Attempted to unregister unknown client PID: {client_pid}")

        logging.debug(f"Saving registry after unregistration, {len(self._registry)} entries remain")
        self._save_registry()

    def _update_child_pids(self, client_pid: int) -> None:
        """Update child PID list for a tracked process.

        This method MUST be called with self.lock held.

        Args:
            client_pid: Client PID to update
        """
        logging.debug(f"Updating child PIDs for client {client_pid}")
        if client_pid not in self._registry:
            logging.debug(f"Client {client_pid} not in registry, skipping child PID update")
            return

        info = self._registry[client_pid]
        logging.debug(f"Querying process tree for root PID {info.root_pid}")

        try:
            # Get root process
            root_proc = psutil.Process(info.root_pid)
            logging.debug(f"Root process {info.root_pid} exists: {root_proc.name()}")

            # Get ALL descendants recursively
            children = root_proc.children(recursive=True)
            old_count = len(info.child_pids)
            info.child_pids = [child.pid for child in children]
            info.last_updated = time.time()

            logging.debug(f"Updated child PIDs for client={client_pid}: {len(info.child_pids)} children (was {old_count})")
            if len(children) > 0:
                logging.debug(f"Child PIDs: {info.child_pids[:10]}{'...' if len(info.child_pids) > 10 else ''}")

        except psutil.NoSuchProcess:
            # Root process died - mark as empty
            info.child_pids = []
            info.last_updated = time.time()
            logging.debug(f"Root process {info.root_pid} no longer exists")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.warning(f"Failed to update child PIDs for client={client_pid}: {e}")

    def refresh_all_child_pids(self) -> None:
        """Refresh child PID lists for all tracked processes."""
        logging.debug(f"Refreshing child PIDs for all {len(self._registry)} tracked processes")
        with self.lock:
            for client_pid in list(self._registry.keys()):
                self._update_child_pids(client_pid)

        logging.debug("Child PID refresh complete for all processes")
        self._save_registry()

    def cleanup_orphaned_processes(self) -> list[int]:
        """Detect and kill process trees for dead clients.

        Returns:
            List of client PIDs that were cleaned up
        """
        logging.debug("Starting orphan cleanup scan")
        logging.debug(f"Checking {len(self._registry)} registered processes for orphans")
        orphaned_clients = []

        with self.lock:
            for client_pid, info in list(self._registry.items()):
                # Check if client is still alive
                logging.debug(f"Checking if client {client_pid} is still alive")
                if psutil.pid_exists(client_pid):
                    logging.debug(f"Client {client_pid} is alive, skipping")
                    continue

                # Client is dead - kill the entire process tree
                logging.info(f"Client {client_pid} is dead, cleaning up process tree (root={info.root_pid}, children={len(info.child_pids)}, operation={info.operation_type})")
                logging.debug(f"Orphan reason: client process {client_pid} terminated")
                logging.debug(f"Process tree age: {time.time() - info.started_at:.1f}s")

                killed_count = self._kill_process_tree(info)
                orphaned_clients.append(client_pid)

                logging.info(f"Cleaned up {killed_count} processes for dead client {client_pid}")
                logging.debug(f"Removing client {client_pid} from registry")

                # Remove from registry
                del self._registry[client_pid]

        logging.debug(f"Orphan cleanup complete: found {len(orphaned_clients)} orphans")
        if orphaned_clients:
            logging.info(f"Orphaned clients cleaned up: {orphaned_clients}")
            logging.debug("Persisting registry after orphan cleanup")
            self._save_registry()
        else:
            logging.debug("No orphaned processes found")

        return orphaned_clients

    def _kill_process_tree(self, info: ProcessTreeInfo) -> int:
        """Kill an entire process tree (root + all children).

        This method MUST be called with self.lock held.

        Args:
            info: ProcessTreeInfo containing root and child PIDs

        Returns:
            Number of processes killed
        """
        logging.debug(f"Killing process tree: root={info.root_pid}, cached_children={len(info.child_pids)}")
        killed_count = 0
        all_pids = info.child_pids + [info.root_pid]

        # Refresh child list one last time before killing
        logging.debug("Refreshing process tree before termination")
        try:
            root_proc = psutil.Process(info.root_pid)
            children = root_proc.children(recursive=True)
            all_pids = [child.pid for child in children] + [info.root_pid]
            logging.debug(f"Refreshed process tree: {len(all_pids)} total processes (root + {len(children)} children)")
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.debug(f"Failed to refresh process tree, using cached list: {e}")
            pass  # Use cached PID list

        # Kill children first (bottom-up to avoid orphans)
        logging.debug("Building process list for termination (bottom-up order)")
        processes_to_kill: list[psutil.Process] = []
        for pid in reversed(all_pids):  # Reverse to kill children before parents
            try:
                proc = psutil.Process(pid)
                processes_to_kill.append(proc)
            except psutil.NoSuchProcess:
                logging.debug(f"Process {pid} already terminated")
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
                logging.debug(f"Terminated process {proc.pid} ({proc.name()})")
            except psutil.NoSuchProcess:
                pass  # Already dead
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                logging.warning(f"Failed to terminate process {proc.pid}: {e}")

        # Wait for graceful termination
        logging.debug("Waiting for processes to terminate gracefully (timeout: 3s)")
        _gone, alive = psutil.wait_procs(processes_to_kill, timeout=3)
        logging.debug(f"Graceful termination complete: {len(_gone)} terminated, {len(alive)} still alive")

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

        logging.debug(f"Process tree termination complete: {killed_count} processes killed")
        return killed_count

    def get_tracked_clients(self) -> list[int]:
        """Get list of all tracked client PIDs.

        Returns:
            List of client PIDs currently being tracked
        """
        logging.debug("Querying tracked client PIDs")
        with self.lock:
            clients = list(self._registry.keys())
        logging.debug(f"Found {len(clients)} tracked clients: {clients}")
        return clients

    def get_process_info(self, client_pid: int) -> ProcessTreeInfo | None:
        """Get process tree info for a client.

        Args:
            client_pid: Client PID to query

        Returns:
            ProcessTreeInfo if found, None otherwise
        """
        logging.debug(f"Querying process info for client {client_pid}")
        with self.lock:
            info = self._registry.get(client_pid)
        if info:
            logging.debug(f"Found process info: root={info.root_pid}, children={len(info.child_pids)}")
        else:
            logging.debug(f"No process info found for client {client_pid}")
        return info

    def get_processes_by_port(self, port: str) -> list[ProcessTreeInfo]:
        """Get all processes using a specific serial port.

        Args:
            port: Serial port to search for

        Returns:
            List of ProcessTreeInfo for processes using this port
        """
        logging.debug(f"Querying processes using port: {port}")
        with self.lock:
            matches = [info for info in self._registry.values() if info.port == port]
        logging.debug(f"Found {len(matches)} processes using port {port}")
        if matches:
            logging.debug(f"Port {port} used by client PIDs: {[info.client_pid for info in matches]}")
        return matches

    def get_processes_by_project(self, project_dir: str) -> list[ProcessTreeInfo]:
        """Get all processes for a specific project.

        Args:
            project_dir: Project directory to search for

        Returns:
            List of ProcessTreeInfo for processes in this project
        """
        logging.debug(f"Querying processes for project: {project_dir}")
        with self.lock:
            matches = [info for info in self._registry.values() if info.project_dir == project_dir]
        logging.debug(f"Found {len(matches)} processes for project {project_dir}")
        if matches:
            logging.debug(f"Project processes: {[(info.client_pid, info.operation_type) for info in matches]}")
        return matches
