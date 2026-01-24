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

from fbuild.daemon.cancellation import (
    OperationCancelledException,
    check_and_raise_if_cancelled,
)
from fbuild.daemon.lock_manager import LockAcquisitionError
from fbuild.daemon.messages import DaemonState, OperationType

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext
    from fbuild.daemon.messages import BuildRequest, DeployRequest, MonitorRequest


# Mapping from OperationType to DaemonState for operation start
_OPERATION_TO_STATE: dict[OperationType, DaemonState] = {
    OperationType.BUILD: DaemonState.BUILDING,
    OperationType.DEPLOY: DaemonState.DEPLOYING,
    OperationType.BUILD_AND_DEPLOY: DaemonState.DEPLOYING,
    OperationType.MONITOR: DaemonState.MONITORING,
    OperationType.INSTALL_DEPENDENCIES: DaemonState.BUILDING,
}

# Mapping from OperationType to message templates
_OPERATION_MESSAGES: dict[OperationType, dict[str, str]] = {
    OperationType.BUILD: {
        "starting": "Building {env}",
        "success": "Build successful",
        "failure": "Build failed",
    },
    OperationType.DEPLOY: {
        "starting": "Deploying {env}",
        "success": "Deploy successful",
        "failure": "Deploy failed",
    },
    OperationType.BUILD_AND_DEPLOY: {
        "starting": "Deploying {env}",
        "success": "Deploy successful",
        "failure": "Deploy failed",
    },
    OperationType.MONITOR: {
        "starting": "Monitoring {env}",
        "success": "Monitor completed",
        "failure": "Monitor failed",
    },
    OperationType.INSTALL_DEPENDENCIES: {
        "starting": "Installing dependencies for {env}",
        "success": "Dependencies installed",
        "failure": "Dependency installation failed",
    },
}


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
        # We store the result to return after lock release and status update
        result: bool = False
        exception_to_reraise: BaseException | None = None

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

                # Check for cancellation before starting operation
                check_and_raise_if_cancelled(
                    context.cancellation_registry,
                    request.request_id,
                    request.caller_pid,
                    self.get_operation_type().value,
                )

                # Execute the operation (implemented by subclass)
                logging.debug(f"[REQUEST_PROCESSOR] Starting execute_operation for {request.request_id}")
                success = self.execute_operation(request, context)
                logging.debug(f"[REQUEST_PROCESSOR] execute_operation returned success={success}")

                # Update final status
                if success:
                    logging.debug("[REQUEST_PROCESSOR] Updating status to COMPLETED")
                    self._update_status(
                        context,
                        DaemonState.COMPLETED,
                        self.get_success_message(request),
                        request=request,
                        exit_code=0,
                        operation_in_progress=False,
                    )
                    logging.debug("[REQUEST_PROCESSOR] Status updated to COMPLETED")
                else:
                    logging.debug("[REQUEST_PROCESSOR] Updating status to FAILED")
                    self._update_status(
                        context,
                        DaemonState.FAILED,
                        self.get_failure_message(request),
                        request=request,
                        exit_code=1,
                        operation_in_progress=False,
                    )
                    logging.debug("[REQUEST_PROCESSOR] Status updated to FAILED")

                result = success

            except OperationCancelledException as ce:
                # Handle cancellation gracefully
                logging.info(f"Operation cancelled: {ce}")

                # Cancel pending compilation jobs if any
                if context.compilation_queue:
                    cancelled_count = context.compilation_queue.cancel_all_jobs()
                    if cancelled_count > 0:
                        logging.info(f"Cancelled {cancelled_count} pending compilation jobs")

                # Update status to CANCELLED
                self._update_status(
                    context,
                    DaemonState.CANCELLED,
                    str(ce),
                    request=request,
                    exit_code=130,  # Standard cancellation exit code (128 + SIGINT)
                    operation_in_progress=False,
                )

                # Clean up signal file
                context.cancellation_registry.cleanup_signal_file(request.request_id)

                result = False

            except KeyboardInterrupt as ki:
                import _thread

                _thread.interrupt_main()
                exception_to_reraise = ki
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
                result = False
            finally:
                # Mark operation complete
                with context.operation_lock:
                    context.operation_in_progress = False

        # After locks are released (ExitStack has exited), update status to reflect
        # the new lock state. This ensures the status file shows locks as released.
        # We read the current status and re-write it to capture the updated lock state.
        logging.debug(f"[REQUEST_PROCESSOR] Locks released, updating status for lock state (result={result})")
        try:
            current_status = context.status_manager.read_status()
            logging.debug(f"[REQUEST_PROCESSOR] Read current status: state={current_status.state}, message={current_status.message}")
            context.status_manager.update_status(
                state=current_status.state,
                message=current_status.message,
                environment=getattr(current_status, "environment", request.environment),
                project_dir=getattr(current_status, "project_dir", request.project_dir),
                request_id=getattr(current_status, "request_id", request.request_id),
                caller_pid=getattr(current_status, "caller_pid", request.caller_pid),
                caller_cwd=getattr(current_status, "caller_cwd", request.caller_cwd),
                exit_code=getattr(current_status, "exit_code", None),
            )
        except KeyboardInterrupt as ke:
            import _thread

            _thread.interrupt_main()
            raise ke
        except Exception as e:
            logging.warning(f"Failed to update status after lock release: {e}")

        # Re-raise KeyboardInterrupt if it was caught
        if exception_to_reraise is not None:
            import _thread

            _thread.interrupt_main()
            raise exception_to_reraise

        logging.debug(f"[REQUEST_PROCESSOR] process_request completed, returning result={result}")
        return result

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

        Returns:
            DaemonState enum value for operation start
        """
        return _OPERATION_TO_STATE.get(self.get_operation_type(), DaemonState.BUILDING)

    def get_starting_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message when operation starts.

        Args:
            request: The request being processed

        Returns:
            Human-readable status message
        """
        messages = _OPERATION_MESSAGES.get(self.get_operation_type())
        template = messages["starting"] if messages else "Processing {env}"
        return template.format(env=request.environment)

    def get_success_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message on success.

        Args:
            request: The request that was processed

        Returns:
            Human-readable success message
        """
        messages = _OPERATION_MESSAGES.get(self.get_operation_type())
        return messages["success"] if messages else "Operation successful"

    def get_failure_message(self, request: "BuildRequest | DeployRequest | MonitorRequest") -> str:
        """Get the status message on failure.

        Args:
            request: The request that failed

        Returns:
            Human-readable failure message
        """
        messages = _OPERATION_MESSAGES.get(self.get_operation_type())
        return messages["failure"] if messages else "Operation failed"

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
        operation_type = self.get_operation_type()
        operation_desc = f"{operation_type.value} for {request.environment}"

        # Acquire project lock if needed
        if "project" in required_locks:
            project_dir = required_locks["project"]
            try:
                lock_stack.enter_context(
                    context.lock_manager.acquire_project_lock(
                        project_dir,
                        blocking=False,
                        operation_id=request.request_id,
                        description=operation_desc,
                    )
                )
            except LockAcquisitionError as e:
                logging.warning(f"Project lock unavailable: {e}")
                self._update_status(
                    context,
                    DaemonState.FAILED,
                    str(e),
                    request=request,
                )
                return False

        # Acquire port lock if needed
        if "port" in required_locks:
            port = required_locks["port"]
            if port:  # Only acquire if port is not None/empty
                try:
                    lock_stack.enter_context(
                        context.lock_manager.acquire_port_lock(
                            port,
                            blocking=False,
                            operation_id=request.request_id,
                            description=operation_desc,
                        )
                    )
                except LockAcquisitionError as e:
                    logging.warning(f"Port lock unavailable: {e}")
                    self._update_status(
                        context,
                        DaemonState.FAILED,
                        str(e),
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
