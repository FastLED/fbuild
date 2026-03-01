"""
MCP (Model Context Protocol) Server package for fbuild Daemon.

Exposes tools, resources, and prompts so AI assistants (Claude Desktop,
Cursor, VS Code) can query and control daemon state.

The FastMCP instance is created here and sub-modules register their
handlers via decorators on import.
"""

from __future__ import annotations

from mcp.server.fastmcp import FastMCP

mcp = FastMCP("fbuild")

# Import sub-modules to register their @mcp.tool / @mcp.resource / @mcp.prompt decorators
import fbuild.daemon.mcp.prompts  # noqa: F401, E402
import fbuild.daemon.mcp.resources  # noqa: F401, E402
import fbuild.daemon.mcp.tools_action  # noqa: F401, E402
import fbuild.daemon.mcp.tools_query  # noqa: F401, E402

__all__ = ["mcp"]
