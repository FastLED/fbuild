"""
WebSocket endpoints for real-time communication with fbuild daemon.

This module provides WebSocket endpoints for:
1. Real-time status updates during builds, deploys, and monitors
2. Bidirectional serial monitor sessions
3. Live daemon log streaming

Architecture:
- Connection manager tracks active WebSocket connections
- Integrates with existing async server broadcast mechanism
- Supports subscription-based event filtering
- Graceful connection/disconnection handling
"""

from __future__ import annotations

import asyncio
import json
import logging
from collections.abc import Callable
from typing import TYPE_CHECKING, Any

from fastapi import APIRouter, Depends, WebSocket, WebSocketDisconnect
from pydantic import BaseModel

if TYPE_CHECKING:
    from fbuild.daemon.daemon_context import DaemonContext


# WebSocket connection tracking
class ConnectionManager:
    """Manages active WebSocket connections and broadcasts.

    This manager tracks active connections by type (status, monitor, logs)
    and provides methods for broadcasting messages to subscribed clients.
    """

    def __init__(self) -> None:
        """Initialize connection manager."""
        # Track active connections by type
        self._status_connections: list[WebSocket] = []
        self._monitor_sessions: dict[str, WebSocket] = {}  # session_id -> websocket
        self._log_connections: list[WebSocket] = []
        self._lock = asyncio.Lock()

    async def connect_status(self, websocket: WebSocket) -> None:
        """Register a status updates WebSocket connection.

        Args:
            websocket: WebSocket connection to register
        """
        await websocket.accept()
        async with self._lock:
            self._status_connections.append(websocket)
        logging.info(f"Status WebSocket connected (total: {len(self._status_connections)})")

    async def disconnect_status(self, websocket: WebSocket) -> None:
        """Unregister a status updates WebSocket connection.

        Args:
            websocket: WebSocket connection to unregister
        """
        async with self._lock:
            if websocket in self._status_connections:
                self._status_connections.remove(websocket)
        logging.info(f"Status WebSocket disconnected (remaining: {len(self._status_connections)})")

    async def connect_monitor(self, session_id: str, websocket: WebSocket) -> None:
        """Register a serial monitor WebSocket connection.

        Args:
            session_id: Unique session identifier
            websocket: WebSocket connection to register
        """
        await websocket.accept()
        async with self._lock:
            self._monitor_sessions[session_id] = websocket
        logging.info(f"Monitor WebSocket connected: {session_id} (total: {len(self._monitor_sessions)})")

    async def disconnect_monitor(self, session_id: str) -> None:
        """Unregister a serial monitor WebSocket connection.

        Args:
            session_id: Session identifier
        """
        async with self._lock:
            if session_id in self._monitor_sessions:
                del self._monitor_sessions[session_id]
        logging.info(f"Monitor WebSocket disconnected: {session_id} (remaining: {len(self._monitor_sessions)})")

    async def connect_logs(self, websocket: WebSocket) -> None:
        """Register a log streaming WebSocket connection.

        Args:
            websocket: WebSocket connection to register
        """
        await websocket.accept()
        async with self._lock:
            self._log_connections.append(websocket)
        logging.info(f"Logs WebSocket connected (total: {len(self._log_connections)})")

    async def disconnect_logs(self, websocket: WebSocket) -> None:
        """Unregister a log streaming WebSocket connection.

        Args:
            websocket: WebSocket connection to unregister
        """
        async with self._lock:
            if websocket in self._log_connections:
                self._log_connections.remove(websocket)
        logging.info(f"Logs WebSocket disconnected (remaining: {len(self._log_connections)})")

    async def broadcast_status(self, message: dict[str, Any]) -> int:
        """Broadcast a status update to all connected status WebSockets.

        Args:
            message: Status update message to broadcast

        Returns:
            Number of clients message was sent to
        """
        disconnected: list[WebSocket] = []
        sent_count = 0

        async with self._lock:
            connections = list(self._status_connections)

        for websocket in connections:
            try:
                await websocket.send_json(message)
                sent_count += 1
            except KeyboardInterrupt:
                raise
            except Exception as e:
                logging.debug(f"Failed to send status update: {e}")
                disconnected.append(websocket)

        # Clean up disconnected clients
        if disconnected:
            async with self._lock:
                for ws in disconnected:
                    if ws in self._status_connections:
                        self._status_connections.remove(ws)

        return sent_count

    async def send_monitor_data(self, session_id: str, data: str | bytes) -> bool:
        """Send data to a specific monitor session.

        Args:
            session_id: Monitor session identifier
            data: Data to send (string or bytes)

        Returns:
            True if sent successfully, False otherwise
        """
        async with self._lock:
            websocket = self._monitor_sessions.get(session_id)

        if not websocket:
            return False

        try:
            if isinstance(data, bytes):
                await websocket.send_bytes(data)
            else:
                await websocket.send_text(data)
            return True
        except KeyboardInterrupt:
            raise
        except Exception as e:
            logging.debug(f"Failed to send monitor data to {session_id}: {e}")
            await self.disconnect_monitor(session_id)
            return False

    async def broadcast_logs(self, log_entry: dict[str, Any]) -> int:
        """Broadcast a log entry to all connected log WebSockets.

        Args:
            log_entry: Log entry to broadcast

        Returns:
            Number of clients message was sent to
        """
        disconnected: list[WebSocket] = []
        sent_count = 0

        async with self._lock:
            connections = list(self._log_connections)

        for websocket in connections:
            try:
                await websocket.send_json(log_entry)
                sent_count += 1
            except KeyboardInterrupt:
                raise
            except Exception as e:
                logging.debug(f"Failed to send log entry: {e}")
                disconnected.append(websocket)

        # Clean up disconnected clients
        if disconnected:
            async with self._lock:
                for ws in disconnected:
                    if ws in self._log_connections:
                        self._log_connections.remove(ws)

        return sent_count

    def get_stats(self) -> dict[str, Any]:
        """Get connection statistics.

        Returns:
            Dictionary with connection counts
        """
        return {
            "status_connections": len(self._status_connections),
            "monitor_sessions": len(self._monitor_sessions),
            "log_connections": len(self._log_connections),
            "total_connections": len(self._status_connections) + len(self._monitor_sessions) + len(self._log_connections),
        }


# Global connection manager (initialized when router is created)
_connection_manager: ConnectionManager | None = None


def get_connection_manager() -> ConnectionManager:
    """Get the global WebSocket connection manager.

    Returns:
        ConnectionManager instance

    Raises:
        RuntimeError: If connection manager not initialized
    """
    global _connection_manager
    if _connection_manager is None:
        _connection_manager = ConnectionManager()
    return _connection_manager


# Pydantic models for WebSocket messages


class StatusUpdateMessage(BaseModel):
    """Status update message sent via WebSocket."""

    type: str = "status"
    state: str  # DaemonState value
    message: str
    current_operation: str | None = None
    operation_in_progress: bool
    progress_percent: float | None = None
    timestamp: float


class MonitorDataMessage(BaseModel):
    """Serial monitor data message."""

    type: str = "monitor_data"
    session_id: str
    data: str
    timestamp: float


class LogEntryMessage(BaseModel):
    """Log entry message."""

    type: str = "log"
    level: str
    message: str
    timestamp: float
    module: str | None = None


class ErrorMessage(BaseModel):
    """Error message."""

    type: str = "error"
    error: str
    detail: str | None = None


# WebSocket endpoint handlers


def create_websockets_router(get_daemon_context_dep: Callable[[], DaemonContext]) -> APIRouter:
    """Create the WebSocket endpoints router.

    Args:
        get_daemon_context_dep: Dependency injection function for DaemonContext

    Returns:
        FastAPI router with WebSocket endpoints
    """
    router = APIRouter(prefix="/ws", tags=["WebSocket"])

    @router.websocket("/status")
    async def websocket_status(  # type: ignore[reportUnusedFunction]
        websocket: WebSocket,
        context: DaemonContext = Depends(get_daemon_context_dep),
    ) -> None:
        """WebSocket endpoint for real-time status updates.

        Clients can connect to this endpoint to receive real-time updates about:
        - Build progress
        - Deploy progress
        - Monitor status
        - Daemon state changes

        Message format:
        {
            "type": "status",
            "state": "building",
            "message": "Compiling src/main.cpp...",
            "current_operation": "build",
            "operation_in_progress": true,
            "progress_percent": 45.5,
            "timestamp": 1234567890.123
        }
        """
        manager = get_connection_manager()
        await manager.connect_status(websocket)

        try:
            # Send initial status
            status = context.status_manager.read_status()
            initial_message = StatusUpdateMessage(
                state=status.state.value,
                message=status.message,
                current_operation=status.current_operation,
                operation_in_progress=status.operation_in_progress,
                progress_percent=getattr(status, "progress_percent", None),
                timestamp=status.updated_at,
            )
            await websocket.send_json(initial_message.model_dump())

            # Keep connection alive and handle incoming messages
            while True:
                try:
                    # Wait for messages from client (ping/pong, etc.)
                    data = await websocket.receive_text()

                    # Parse client message
                    try:
                        client_msg = json.loads(data)
                        msg_type = client_msg.get("type")

                        if msg_type == "ping":
                            # Respond with pong
                            await websocket.send_json({"type": "pong", "timestamp": asyncio.get_event_loop().time()})
                        elif msg_type == "get_status":
                            # Send current status
                            status = context.status_manager.read_status()
                            status_msg = StatusUpdateMessage(
                                state=status.state.value,
                                message=status.message,
                                current_operation=status.current_operation,
                                operation_in_progress=status.operation_in_progress,
                                progress_percent=getattr(status, "progress_percent", None),
                                timestamp=status.updated_at,
                            )
                            await websocket.send_json(status_msg.model_dump())
                    except json.JSONDecodeError:
                        await websocket.send_json(ErrorMessage(error="Invalid JSON", detail="Could not parse message").model_dump())

                except WebSocketDisconnect:
                    break
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"Error in status WebSocket: {e}")
                    break

        finally:
            await manager.disconnect_status(websocket)

    @router.websocket("/monitor/{session_id}")
    async def websocket_monitor(  # type: ignore[reportUnusedFunction]
        websocket: WebSocket,
        session_id: str,
        context: DaemonContext = Depends(get_daemon_context_dep),
    ) -> None:
        """WebSocket endpoint for serial monitor sessions.

        This endpoint provides bidirectional communication with a serial device:
        - Receive data from the serial port
        - Send data to the serial port

        Args:
            session_id: Unique identifier for this monitor session

        Client -> Server message format:
        {
            "type": "write",
            "data": "Hello device\\n"
        }

        Server -> Client message format:
        {
            "type": "monitor_data",
            "session_id": "abc123",
            "data": "Output from device",
            "timestamp": 1234567890.123
        }
        """
        manager = get_connection_manager()
        await manager.connect_monitor(session_id, websocket)

        try:
            # Send welcome message
            welcome = MonitorDataMessage(
                session_id=session_id,
                data=f"Connected to monitor session: {session_id}\n",
                timestamp=asyncio.get_event_loop().time(),
            )
            await websocket.send_json(welcome.model_dump())

            # Handle incoming/outgoing data
            while True:
                try:
                    data = await websocket.receive_text()

                    # Parse client message
                    try:
                        client_msg = json.loads(data)
                        msg_type = client_msg.get("type")

                        if msg_type == "write":
                            # Client wants to write data to serial port
                            write_data = client_msg.get("data", "")
                            if write_data and context.shared_serial_manager:
                                # TODO: Implement serial write via shared_serial_manager
                                # For now, echo back as acknowledgment
                                ack = {"type": "ack", "timestamp": asyncio.get_event_loop().time()}
                                await websocket.send_json(ack)
                        elif msg_type == "ping":
                            await websocket.send_json({"type": "pong", "timestamp": asyncio.get_event_loop().time()})
                    except json.JSONDecodeError:
                        await websocket.send_json(ErrorMessage(error="Invalid JSON", detail="Could not parse message").model_dump())

                except WebSocketDisconnect:
                    break
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"Error in monitor WebSocket: {e}")
                    break

        finally:
            await manager.disconnect_monitor(session_id)

    @router.websocket("/logs")
    async def websocket_logs(  # type: ignore[reportUnusedFunction]
        websocket: WebSocket,
        context: DaemonContext = Depends(get_daemon_context_dep),
    ) -> None:
        """WebSocket endpoint for live daemon log streaming.

        Clients can connect to this endpoint to receive real-time log entries
        from the daemon. Useful for debugging and monitoring daemon activity.

        Message format:
        {
            "type": "log",
            "level": "INFO",
            "message": "Build completed successfully",
            "timestamp": 1234567890.123,
            "module": "build_processor"
        }
        """
        manager = get_connection_manager()
        await manager.connect_logs(websocket)

        try:
            # Send welcome message
            welcome = LogEntryMessage(
                level="INFO",
                message="Connected to daemon log stream",
                timestamp=asyncio.get_event_loop().time(),
                module="websockets",
            )
            await websocket.send_json(welcome.model_dump())

            # Keep connection alive
            while True:
                try:
                    data = await websocket.receive_text()

                    # Parse client message (ping/pong only)
                    try:
                        client_msg = json.loads(data)
                        if client_msg.get("type") == "ping":
                            await websocket.send_json({"type": "pong", "timestamp": asyncio.get_event_loop().time()})
                    except json.JSONDecodeError:
                        pass

                except WebSocketDisconnect:
                    break
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"Error in logs WebSocket: {e}")
                    break

        finally:
            await manager.disconnect_logs(websocket)

    @router.websocket("/serial-monitor")
    async def websocket_serial_monitor_api(  # type: ignore[reportUnusedFunction]
        websocket: WebSocket,
        context: DaemonContext = Depends(get_daemon_context_dep),
    ) -> None:
        """WebSocket endpoint for fbuild.api.SerialMonitor API.

        This endpoint replaces the file-based IPC for the SerialMonitor Python API.
        It provides real-time bidirectional serial communication with automatic
        preemption handling and reconnection.

        Client -> Server messages:
        {
            "type": "attach",
            "client_id": "serial_monitor_abc123",
            "port": "COM13",
            "baud_rate": 115200,
            "open_if_needed": true
        }
        {
            "type": "write",
            "data": "base64_encoded_data"
        }
        {
            "type": "detach"
        }

        Server -> Client messages:
        {
            "type": "attached",
            "success": true,
            "message": "Attached to COM13"
        }
        {
            "type": "data",
            "lines": ["line1", "line2", ...],
            "current_index": 42
        }
        {
            "type": "preempted",
            "reason": "deploy",
            "preempted_by": "deploy_client_xyz"
        }
        {
            "type": "reconnected",
            "message": "Port available, reattached"
        }
        {
            "type": "write_ack",
            "bytes_written": 10
        }
        {
            "type": "error",
            "message": "Error description"
        }
        """
        await websocket.accept()

        # Import processor for serial monitor operations
        import uuid

        from fbuild.daemon.messages import (
            SerialMonitorAttachRequest,
            SerialMonitorDetachRequest,
            SerialMonitorPollRequest,
            SerialWriteRequest,
        )
        from fbuild.daemon.processors.serial_monitor_processor import (
            SerialMonitorAPIProcessor,
        )

        processor = SerialMonitorAPIProcessor()
        client_id: str | None = None
        port: str | None = None
        attached = False
        last_index = 0

        # Message queue for incoming messages
        message_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
        running = True

        # Background task for receiving messages from client
        async def message_receiver():
            """Background task that receives messages from client and queues them."""
            nonlocal running
            try:
                while running:
                    try:
                        data = await websocket.receive_text()
                        msg = json.loads(data)
                        await message_queue.put(msg)
                    except json.JSONDecodeError:
                        await websocket.send_json(
                            {
                                "type": "error",
                                "message": "Invalid JSON",
                            }
                        )
                    except WebSocketDisconnect:
                        running = False
                        break
                    except KeyboardInterrupt:
                        raise
                    except Exception as e:
                        logging.error(f"Error in message receiver: {e}")
                        running = False
                        break
            finally:
                # Signal shutdown by putting a sentinel value
                await message_queue.put({"type": "_shutdown"})

        # Background task for processing messages from queue
        async def message_processor():
            """Background task that processes messages from queue."""
            nonlocal client_id, port, attached, last_index, running

            while running:
                try:
                    # Check shutdown state early - avoid run_in_executor calls if shutting down
                    if context.is_shutting_down:
                        logging.debug("message_processor detected shutdown, exiting")
                        break

                    # Get message from queue with timeout
                    try:
                        msg = await asyncio.wait_for(message_queue.get(), timeout=1.0)
                    except asyncio.TimeoutError:
                        continue

                    msg_type = msg.get("type")
                    logging.info(f"[WS_DEBUG] message_processor received: type={msg_type}")

                    # Check for shutdown sentinel
                    if msg_type == "_shutdown":
                        break

                    if msg_type == "attach":
                        # Handle attach request
                        client_id = msg.get("client_id", f"ws_monitor_{uuid.uuid4().hex[:8]}")
                        port = msg["port"]
                        baud_rate = msg.get("baud_rate", 115200)
                        open_if_needed = msg.get("open_if_needed", True)

                        # Type guard - ensure client_id and port are strings
                        if not isinstance(client_id, str) or not isinstance(port, str):
                            await websocket.send_json(
                                {
                                    "type": "error",
                                    "message": "Invalid client_id or port",
                                }
                            )
                            continue

                        attach_request = SerialMonitorAttachRequest(
                            client_id=client_id,
                            port=port,
                            baud_rate=baud_rate,
                            open_if_needed=open_if_needed,
                        )

                        # Use thread pool to avoid blocking async loop
                        loop = asyncio.get_event_loop()
                        response = await loop.run_in_executor(
                            None,
                            processor.handle_attach,
                            attach_request,
                            context,
                        )

                        if response.success:
                            attached = True
                            last_index = 0

                        # Send response immediately
                        await websocket.send_json(
                            {
                                "type": "attached",
                                "success": response.success,
                                "message": response.message,
                            }
                        )

                    elif msg_type == "write":
                        # Handle write request
                        logging.info(f"[WS_DEBUG] Write message received, attached={attached}, port={port}")
                        if not attached:
                            await websocket.send_json(
                                {
                                    "type": "error",
                                    "message": "Not attached to port",
                                }
                            )
                            continue

                        data_b64 = msg.get("data", "")

                        write_request = SerialWriteRequest(
                            client_id=client_id or "",
                            port=port or "",
                            data=data_b64,
                            acquire_writer=True,
                        )

                        # Use thread pool with timeout to prevent blocking forever
                        # With serial write retry logic (3 attempts at 5s each + delays),
                        # max time is ~18s. Use 20s to be safe.
                        loop = asyncio.get_event_loop()
                        try:
                            response = await asyncio.wait_for(
                                loop.run_in_executor(
                                    None,
                                    processor.handle_write,
                                    write_request,
                                    context,
                                ),
                                timeout=20.0,  # Allow for serial write retries (3 x 5s + delays)
                            )
                        except asyncio.TimeoutError:
                            # Write operation timed out - release writer and send error
                            if context.shared_serial_manager:
                                context.shared_serial_manager.release_writer(port or "", client_id or "")
                            try:
                                await websocket.send_json(
                                    {
                                        "type": "write_ack",
                                        "success": False,
                                        "bytes_written": 0,
                                        "message": "Write operation timed out",
                                    }
                                )
                            except KeyboardInterrupt:
                                raise
                            except Exception:
                                # WebSocket may have been closed, ignore
                                logging.warning("Could not send write_ack (WebSocket closed)")
                            continue

                        # Send write response - WebSocket may have been closed during write
                        try:
                            await websocket.send_json(
                                {
                                    "type": "write_ack",
                                    "success": response.success,
                                    "bytes_written": response.bytes_written,
                                    "message": response.message,
                                }
                            )
                        except KeyboardInterrupt:
                            raise
                        except Exception:
                            # WebSocket may have been closed, ignore
                            logging.warning("Could not send write_ack (WebSocket closed)")

                    elif msg_type == "detach":
                        # Handle detach request
                        if attached and client_id and port:
                            detach_request = SerialMonitorDetachRequest(
                                client_id=client_id,
                                port=port,
                            )

                            # Use thread pool
                            loop = asyncio.get_event_loop()
                            response = await loop.run_in_executor(
                                None,
                                processor.handle_detach,
                                detach_request,
                                context,
                            )

                            attached = False

                            await websocket.send_json(
                                {
                                    "type": "detached",
                                    "success": response.success,
                                    "message": response.message,
                                }
                            )

                    elif msg_type == "ping":
                        await websocket.send_json({"type": "pong", "timestamp": asyncio.get_event_loop().time()})

                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"Error in message processor: {e}")
                    try:
                        await websocket.send_json(
                            {
                                "type": "error",
                                "message": str(e),
                            }
                        )
                    except KeyboardInterrupt:
                        raise
                    except Exception:
                        pass

        # Background task for pushing data to client
        pusher_task: asyncio.Task | None = None

        async def data_pusher():
            """Background task that polls serial manager and pushes data to client."""
            nonlocal last_index
            while True:
                try:
                    # Check shutdown state - avoid run_in_executor calls if shutting down
                    if context.is_shutting_down:
                        logging.debug("data_pusher detected shutdown, exiting")
                        break

                    if not attached or not port:
                        # Not attached, sleep and check again
                        await asyncio.sleep(0.1)
                        continue

                    # Poll for new data
                    poll_request = SerialMonitorPollRequest(
                        client_id=client_id or "",
                        port=port,
                        last_index=last_index,
                        max_lines=1000,
                    )

                    # Use thread pool to avoid blocking async loop
                    loop = asyncio.get_event_loop()
                    response = await loop.run_in_executor(
                        None,
                        processor.handle_poll,
                        poll_request,
                        context,
                    )

                    if response.success and response.lines:
                        # Send data to client
                        await websocket.send_json(
                            {
                                "type": "data",
                                "lines": response.lines,
                                "current_index": response.current_index,
                            }
                        )
                        last_index = response.current_index

                    # Check for preemption (TODO: add preemption detection)

                    # Sleep before next poll (100ms like file-based version)
                    await asyncio.sleep(0.1)

                except asyncio.CancelledError:
                    # Task was cancelled, exit cleanly
                    break
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    # Check if WebSocket is closed (ASGI error indicates closed connection)
                    error_str = str(e)
                    if "websocket.close" in error_str or "response already completed" in error_str:
                        # WebSocket is closed, exit the pusher loop
                        logging.debug("Data pusher detected WebSocket closed, exiting")
                        break
                    logging.error(f"Error in serial monitor data pusher: {e}")
                    await asyncio.sleep(0.1)

        # Start data pusher as a background task
        pusher_task = asyncio.create_task(data_pusher())

        try:
            # Run receiver and processor concurrently (they will handle shutdown via message queue)
            await asyncio.gather(
                message_receiver(),
                message_processor(),
                return_exceptions=True,
            )

        finally:
            # Cleanup
            running = False
            attached = False

            # Cancel pusher task
            if pusher_task:
                pusher_task.cancel()
                try:
                    await pusher_task
                except KeyboardInterrupt:
                    raise
                except asyncio.CancelledError:
                    pass

            # Release writer lock if held (prevents stale locks on disconnect)
            if client_id and port:
                try:
                    # Directly release writer through shared serial manager
                    # This is necessary because handle_detach doesn't release the writer lock
                    if context.shared_serial_manager:
                        context.shared_serial_manager.release_writer(port, client_id)
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    # Ignore errors (client may not have been writer)
                    logging.debug(f"Writer release in cleanup (may be expected): {e}")

            # Detach from serial session if still attached
            if client_id and port:
                try:
                    detach_request = SerialMonitorDetachRequest(
                        client_id=client_id,
                        port=port,
                    )

                    # Check if daemon is shutting down
                    if context.is_shutting_down:
                        # Run synchronously - executor may be shut down
                        # This avoids "cannot schedule new futures after shutdown" error
                        processor.handle_detach(detach_request, context)
                    else:
                        # Run in executor as normal
                        loop = asyncio.get_event_loop()
                        await loop.run_in_executor(
                            None,
                            processor.handle_detach,
                            detach_request,
                            context,
                        )
                except KeyboardInterrupt:
                    raise
                except Exception as e:
                    logging.error(f"Error detaching in cleanup: {e}")

    return router


# Helper function for broadcasting status updates (called by processors)


async def broadcast_status_update(
    state: str,
    message: str,
    current_operation: str | None = None,
    progress_percent: float | None = None,
) -> int:
    """Broadcast a status update to all connected WebSocket clients.

    This function is called by processors (build, deploy, monitor) to send
    real-time updates to clients.

    Args:
        state: Current daemon state (DaemonState value)
        message: Status message
        current_operation: Optional current operation description
        progress_percent: Optional progress percentage (0-100)

    Returns:
        Number of clients message was sent to
    """
    manager = get_connection_manager()

    update = StatusUpdateMessage(
        state=state,
        message=message,
        current_operation=current_operation,
        operation_in_progress=True,
        progress_percent=progress_percent,
        timestamp=asyncio.get_event_loop().time(),
    )

    return await manager.broadcast_status(update.model_dump())


async def broadcast_log_entry(
    level: str,
    message: str,
    module: str | None = None,
) -> int:
    """Broadcast a log entry to all connected WebSocket clients.

    Args:
        level: Log level (DEBUG, INFO, WARNING, ERROR, CRITICAL)
        message: Log message
        module: Optional module name

    Returns:
        Number of clients message was sent to
    """
    manager = get_connection_manager()

    entry = LogEntryMessage(
        level=level,
        message=message,
        timestamp=asyncio.get_event_loop().time(),
        module=module,
    )

    return await manager.broadcast_logs(entry.model_dump())
