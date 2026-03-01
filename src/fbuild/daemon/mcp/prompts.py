"""
MCP prompt templates for the fbuild daemon.

Prompts return structured text that helps AI assistants construct
good diagnostic queries.
"""

from __future__ import annotations

from fbuild.daemon.mcp import mcp

# ---------------------------------------------------------------------------
# 1. diagnose_build_failure — gather build errors, recent ops, stale locks
# ---------------------------------------------------------------------------


@mcp.prompt()
def diagnose_build_failure(project_dir: str | None = None) -> str:
    """Gather diagnostic information for a build failure.

    Collects build errors, recent operations, and stale lock warnings
    into a markdown-formatted diagnostic report.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    sections: list[str] = ["# Build Failure Diagnostic Report\n"]

    # --- Build errors ---
    collector = context.error_collector
    if collector.has_errors() or collector.has_warnings():
        sections.append("## Build Errors\n")
        sections.append(collector.format_errors(max_errors=20))
        sections.append("")
    else:
        sections.append("## Build Errors\n")
        sections.append("No errors recorded in the error collector.\n")

    # --- Recent operations ---
    registry = context.operation_registry
    if project_dir is not None:
        ops = registry.get_operations_by_project(project_dir)
    else:
        ops = list(registry.operations.values())

    ops.sort(key=lambda op: op.created_at, reverse=True)
    recent = ops[:10]

    if recent:
        sections.append("## Recent Operations\n")
        for op in recent:
            duration = round(op.duration() or 0.0, 1)
            line = f"- **{op.operation_type.value}** `{op.environment}` — {op.state.value} ({duration}s)"
            if op.error_message:
                line += f"\n  Error: {op.error_message}"
            sections.append(line)
        sections.append("")

    # --- Stale locks ---
    stale = context.lock_manager.get_stale_locks()
    if stale.has_stale_locks():
        sections.append("## Stale Lock Warning\n")
        for rl in stale.stale_port_locks:
            sections.append(f"- Stale port lock: `{rl.resource_id}`")
        for rl in stale.stale_project_locks:
            sections.append(f"- Stale project lock: `{rl.resource_id}`")
        sections.append("")
        sections.append("Consider running the `clear_stale_locks` tool to release these.\n")

    return "\n".join(sections)


# ---------------------------------------------------------------------------
# 2. recommend_deploy_target — suggest best device for deployment
# ---------------------------------------------------------------------------


@mcp.prompt()
def recommend_deploy_target(environment: str | None = None) -> str:
    """Recommend which device to deploy firmware to.

    Gathers device inventory, firmware ledger status, and lease info
    to produce a markdown-formatted recommendation.
    """
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    sections: list[str] = ["# Deploy Target Recommendation\n"]

    # --- Device inventory ---
    device_manager = context.device_manager
    all_devices = device_manager.get_all_devices()
    connected = [s for s in all_devices.values() if s.is_connected]
    available = [s for s in connected if s.is_available_for_exclusive()]

    sections.append("## Device Inventory\n")
    sections.append(f"- Total tracked devices: {len(all_devices)}")
    sections.append(f"- Connected: {len(connected)}")
    sections.append(f"- Available for deploy (no exclusive lease): {len(available)}")
    sections.append("")

    if not connected:
        sections.append("**No devices connected.** Plug in a board and run `refresh_devices`.\n")
        return "\n".join(sections)

    # --- Per-device details ---
    sections.append("## Connected Devices\n")
    ledger = context.firmware_ledger
    for state in connected:
        info = state.device_info
        port = info.port
        entry = ledger.get_deployment(port)

        status_parts = [f"**{port}** ({info.description})"]
        if state.exclusive_lease:
            status_parts.append(f"  - Exclusive lease held by `{state.exclusive_lease.client_id}`: {state.exclusive_lease.description}")
        if state.monitor_leases:
            status_parts.append(f"  - {len(state.monitor_leases)} active monitor lease(s)")
        if entry:
            stale_tag = " (STALE)" if entry.is_stale() else ""
            status_parts.append(f"  - Last firmware: env=`{entry.environment}`, project=`{entry.project_dir}`{stale_tag}")
        else:
            status_parts.append("  - No firmware recorded")

        sections.append("\n".join(status_parts))
        sections.append("")

    # --- Recommendation ---
    if available:
        best = available[0]
        sections.append("## Recommendation\n")
        sections.append(f"Deploy to **{best.device_info.port}** — it is connected and has no active exclusive lease.\n")
    else:
        sections.append("## Recommendation\n")
        sections.append("All connected devices have exclusive leases. Wait for the current operation to finish or use `clear_stale_locks` if locks appear stuck.\n")

    return "\n".join(sections)
