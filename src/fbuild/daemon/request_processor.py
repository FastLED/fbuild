"""
Request Processor - Template method pattern for daemon request handling.

This module provides the RequestProcessor abstract base class which implements
the Template Method pattern to eliminate code duplication across build, deploy,
and monitor request handlers. It handles all common concerns (lock management,
status updates, error handling) while allowing subclasses to implement only
the operation-specific business logic.
"""

import logging
import time
from abc import ABC, abstractmethod
from contextlib import ExitStack
from typing import TYPE_CHECKING, Any

from fbuild.daemon.messages import DaemonState, OperationType

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.messages import BuildRequest, DeployRequest, MonitorRequest


class RequestProcessor(ABC):
    """Abstract base class for processing daemon requests.

    This class implements the Template Method pattern to handle all common
    concerns of request processing:
    - Request validation
    - Lock acquisition (port and/or project locks)
    - Status updates (started, in-progress, completed, failed)
    - Error handling and cleanup
    - Operation tracking

    Subclasses only need to implement:
    - get_operation_type(): Return the OperationType
    - get_required_locks(): Specify which locks are needed
    - execute_operation(): Implement the actual business logic

    Example:
        >>> class BuildRequestProcessor(RequestProcessor):
        ...     def get_operation_type(self) -> OperationType:
        ...         return OperationType.BUILD
        ...
        ...     def get_required_locks(self, request, context):
        ...         return {"project": request.project_dir}
        ...
        ...     def execute_operation(self, request, context):
        ...         # Actual build logic here
        ...         result = build_project(request.project_dir)
        ...         return result.success
    """

    def process_request(
        self,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        context: "DaemonContext",
    ) -> bool:
        """Process a request using the template method pattern.

        This is the main entry point that coordinates the entire request
        processing lifecycle. It handles all boilerplate while calling
        abstract methods for operation-specific logic.

        Args:
            request: The request to process (BuildRequest, DeployRequest, or MonitorRequest)
            context: The daemon context containing all subsystems

        Returns:
            True if operation succeeded, False otherwise

        Lifecycle:
            1. Validate request
            2. Acquire required locks (project and/or port)
            3. Mark operation as in progress
            4. Update status to starting state
            5. Execute operation (abstract method)
            6. Update status based on result
            7. Release locks and cleanup

        Example:
            >>> processor = BuildRequestProcessor()
            >>> success = processor.process_request(build_request, daemon_context)
        """
        logging.info(f"Processing {self.get_operation_type().value} request {request.request_id}: " + f"env={request.environment}, project={request.project_dir}")

        # Validate request
        if not self.validate_request(request, context):
            self._update_status(
                context,
                DaemonState.FAILED,
                "Request validation failed",
                request=request,
                exit_code=1,
            )
            return False

        # Use ExitStack to manage multiple locks as context managers
        with ExitStack() as lock_stack:
            # Acquire required locks
            if not self._acquire_locks(request, context, lock_stack):
                return False

            try:
                # Mark operation in progress
                with context.operation_lock:
                    context.operation_in_progress = True

                # Update status to starting state
                self._update_status(
                    context,
                    self.get_starting_state(),
                    self.get_starting_message(request),
                    request=request,
                    request_started_at=time.time(),
                    operation_type=self.get_operation_type(),
                )

                # Execute the operation (implemented by subclass)
                success = self.execute_operation(request, context)

                # Update final status
                if success:
                    self._update_status(
                        context,
                        DaemonState.COMPLETED,
                        self.get_success_message(request),
                        request=request,
                        exit_code=0,
                        operation_in_progress=False,
                    )
                else:
                    self._update_status(
                        context,
                        DaemonState.FAILED,
                        self.get_failure_message(request),
                        request=request,
                        exit_code=1,
                        operation_in_progress=False,
                    )

                return success

            except KeyboardInterrupt:
                import _thread

                _thread.interrupt_main()
                raise
            except Exception as e:
                import traceback

                logging.error(f"{self.get_operation_type().value} exception: {e}")
                logging.error(f"Traceback:\n{traceback.format_exc()}")
                self._update_status(
                    context,
                    DaemonState.FAILED,
                    f"{self.get_operation_type().value} exception: {e}",
                    request=request,
                    exit_code=1,
                    operation_in_progress=False,
                )
                return False
            finally:
                # Mark operation complete
                with context.operation_lock:
                    context.operation_in_progress = False

    @abstractmethod
    def get_operation_type(self) -> OperationType:
        """Get the operation type for this processor.

        Returns:
            OperationType enum value (BUILD, DEPLOY, MONITOR, etc.)
        """
        pass

    @abstractmethod
    def get_required_locks(
        self,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        context: "DaemonContext",
    ) -> dict[str, str]:
        """Specify which locks are required for this operation.

        Returns:
            Dictionary with lock types as keys and resource identifiers as values.
            Valid keys: "project" (for project_dir), "port" (for serial port)

        Examples:
            Build only needs project lock:
                return {"project": request.project_dir}

            Deploy needs both project and port locks:
                return {"project": request.project_dir, "port": request.port}

            Monitor only needs port lock:
                return {"port": request.port}
        """
        pass

    @abstractmethod
    def execute_operation(
        self,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        context: "DaemonContext",
    ) -> bool:
        """Execute the actual operation logic.

        This is the core business logic that subclasses must implement.
        All boilerplate (locks, status updates, error handling) is handled
        by the base class.

        Args:
            request: The request being processed
            context: The daemon context with all subsystems

        Returns:
            True if operation succeeded, False otherwise

        Example:
            >>> def execute_operation(self, request, context):
            ...     # Build the project
            ...     orchestrator = BuildOrchestratorAVR(verbose=request.verbose)
            ...     result = orchestrator.build(
            ...         project_dir=Path(request.project_dir),
            ...         env_name=request.environment,
            ...         clean=request.clean_build,
            ...     )
            ...     return result.success
        """
        pass

    def validate_request(
        self,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        context: "DaemonContext",
    ) -> bool:
        """Validate the request before processing.

        Default implementation always returns True. Override to add validation.

        Args:
            request: The request to validate
            context: The daemon context

        Returns:
            True if request is valid, False otherwise
        """
        return True

    def get_starting_state(self) -> DaemonState:
        """Get the daemon state when operation starts.

        Default implementation uses BUILDING. Override for different operations.

        Returns:
            DaemonState enum value for operation start
        """
        operation_type = self.get_operation_type()
        if operation_type == OperationType.BUILD:
            return DaemonState.BUILDING
        elif operation_type == OperationType.DEPLOY or operation_type == OperationType.BUILD_AND_DEPLOY:
            return DaemonState.DEPLOYING
        elif operation_type == OperationType.MONITOR:
            return DaemonState.MONITORING
        else:
            return DaemonState.BUILDING

    def get_starting_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message when operation starts.

        Args:
            request: The request being processed

        Returns:
            Human-readable status message
        """
        operation_type = self.get_operation_type()
        if operation_type == OperationType.BUILD:
            return f"Building {request.environment}"
        elif operation_type == OperationType.DEPLOY or operation_type == OperationType.BUILD_AND_DEPLOY:
            return f"Deploying {request.environment}"
        elif operation_type == OperationType.MONITOR:
            return f"Monitoring {request.environment}"
        else:
            return f"Processing {request.environment}"

    def get_success_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message on success.

        Args:
            request: The request that was processed

        Returns:
            Human-readable success message
        """
        operation_type = self.get_operation_type()
        if operation_type == OperationType.BUILD:
            return "Build successful"
        elif operation_type == OperationType.DEPLOY or operation_type == OperationType.BUILD_AND_DEPLOY:
            return "Deploy successful"
        elif operation_type == OperationType.MONITOR:
            return "Monitor completed"
        else:
            return "Operation successful"

    def get_failure_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message on failure.

        Args:
            request: The request that failed

        Returns:
            Human-readable failure message
        """
        operation_type = self.get_operation_type()
        if operation_type == OperationType.BUILD:
            return "Build failed"
        elif operation_type == OperationType.DEPLOY or operation_type == OperationType.BUILD_AND_DEPLOY:
            return "Deploy failed"
        elif operation_type == OperationType.MONITOR:
            return "Monitor failed"
        else:
            return "Operation failed"

    def _acquire_locks(
        self,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        context: "DaemonContext",
        lock_stack: ExitStack,
    ) -> bool:
        """Acquire all required locks for the operation.

        Args:
            request: The request being processed
            context: The daemon context
            lock_stack: ExitStack to manage lock lifetimes

        Returns:
            True if all locks acquired, False if any lock is unavailable
        """
        required_locks = self.get_required_locks(request, context)

        # Acquire project lock if needed
        if "project" in required_locks:
            project_dir = required_locks["project"]
            try:
                lock_stack.enter_context(context.lock_manager.acquire_project_lock(project_dir, blocking=False))
            except RuntimeError:
                logging.warning(f"Project {project_dir} is already being built")
                self._update_status(
                    context,
                    DaemonState.FAILED,
                    f"Project {project_dir} is already being built by another process",
                    request=request,
                )
                return False

        # Acquire port lock if needed
        if "port" in required_locks:
            port = required_locks["port"]
            if port:  # Only acquire if port is not None/empty
                try:
                    lock_stack.enter_context(context.lock_manager.acquire_port_lock(port, blocking=False))
                except RuntimeError:
                    logging.warning(f"Port {port} is already in use")
                    self._update_status(
                        context,
                        DaemonState.FAILED,
                        f"Port {port} is already in use by another operation",
                        request=request,
                    )
                    return False

        return True

    def _update_status(
        self,
        context: "DaemonContext",
        state: DaemonState,
        message: str,
        request: "BuildRequest | DeployRequest | MonitorRequest",
        **kwargs: Any,
    ) -> None:
        """Update daemon status file.

        Args:
            context: The daemon context
            state: New daemon state
            message: Status message
            request: The request being processed
            **kwargs: Additional fields for status update
        """
        # Use the status manager from context
        context.status_manager.update_status(
            state=state,
            message=message,
            environment=request.environment,
            project_dir=request.project_dir,
            request_id=request.request_id,
            caller_pid=request.caller_pid,
            caller_cwd=request.caller_cwd,
            **kwargs,
        )
