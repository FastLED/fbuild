"""
MCP resources for the fbuild daemon.

Resources are read-only data surfaces identified by URIs.
"""

from __future__ import annotations

import json
from urllib.parse import unquote

from fbuild.daemon.mcp import mcp

# ---------------------------------------------------------------------------
# 1. fbuild://daemon/log — tail of daemon log (last 200 lines)
# ---------------------------------------------------------------------------


@mcp.resource("fbuild://daemon/log", mime_type="text/plain")
def daemon_log() -> str:
    """Return the last 200 lines of the daemon log file."""
    from fbuild.daemon.paths import LOG_FILE

    try:
        text = LOG_FILE.read_text(encoding="utf-8", errors="replace")
    except KeyboardInterrupt:
        raise
    except Exception:
        return "(daemon log not available)"

    lines = text.splitlines()
    tail = lines[-200:] if len(lines) > 200 else lines
    return "\n".join(tail)


# ---------------------------------------------------------------------------
# 2. fbuild://project/{project_dir}/config — parsed platformio.ini
# ---------------------------------------------------------------------------


@mcp.resource("fbuild://project/{project_dir}/config", mime_type="application/json")
def project_config(project_dir: str) -> str:
    """Return parsed platformio.ini configuration for a project.

    The project_dir parameter is URL-encoded in the URI.
    """
    from pathlib import Path

    from fbuild.config.ini_parser import PlatformIOConfig, PlatformIOConfigError

    decoded_dir = unquote(project_dir)
    ini_path = Path(decoded_dir) / "platformio.ini"

    try:
        config = PlatformIOConfig(ini_path)
    except PlatformIOConfigError as exc:
        return json.dumps({"error": str(exc)})

    environments = config.get_environments()
    env_configs: dict[str, dict] = {}
    for env_name in environments:
        try:
            env_configs[env_name] = config.get_env_config(env_name)
        except KeyboardInterrupt:
            raise
        except Exception as exc:
            env_configs[env_name] = {"error": str(exc)}

    return json.dumps(
        {
            "project_dir": decoded_dir,
            "environments": env_configs,
        },
        indent=2,
    )


# ---------------------------------------------------------------------------
# 3. fbuild://firmware/{port} — firmware info for a serial port
# ---------------------------------------------------------------------------


@mcp.resource("fbuild://firmware/{port}", mime_type="application/json")
def firmware_info(port: str) -> str:
    """Return firmware deployment information for a serial port."""
    from fbuild.daemon.fastapi_app import get_daemon_context

    context = get_daemon_context()
    entry = context.firmware_ledger.get_deployment(port)

    if entry is None:
        return json.dumps({"found": False, "port": port})

    return json.dumps(
        {
            "found": True,
            "port": port,
            "firmware_hash": entry.firmware_hash,
            "source_hash": entry.source_hash,
            "project_dir": entry.project_dir,
            "environment": entry.environment,
            "upload_timestamp": entry.upload_timestamp,
            "is_stale": entry.is_stale(),
        }
    )
