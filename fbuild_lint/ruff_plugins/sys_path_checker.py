"""Flake8 plugin to detect sys.path.insert() calls.

This plugin detects sys.path.insert() calls which usually indicate that the code
is trying to run Python directly without proper virtual environment activation.

Error Codes:
    SPI001: sys.path.insert() detected - use proper virtual environment activation instead
"""

import ast
from typing import Any, Generator, Tuple, Type


class SysPathChecker:
    """Flake8 plugin to detect sys.path.insert() calls."""

    name = "sys-path-checker"
    version = "1.0.0"

    def __init__(self, tree: ast.AST) -> None:
        """Initialize the checker with an AST tree.

        Args:
            tree: The AST tree to check
        """
        self._tree = tree

    def run(self) -> Generator[Tuple[int, int, str, Type[Any]], None, None]:
        """Run the checker on the AST tree.

        Yields:
            Tuple of (line_number, column, message, type)
        """
        visitor = SysPathVisitor()
        visitor.visit(self._tree)

        for line, col, msg in visitor.errors:
            yield (line, col, msg, type(self))


class SysPathVisitor(ast.NodeVisitor):
    """AST visitor to detect sys.path.insert() calls."""

    def __init__(self) -> None:
        """Initialize the visitor."""
        self.errors: list[Tuple[int, int, str]] = []

    def visit_Call(self, node: ast.Call) -> None:
        """Visit a Call node and check if it's sys.path.insert().

        Args:
            node: The Call node to check
        """
        # Check if this is a call to sys.path.insert()
        if isinstance(node.func, ast.Attribute):
            # Check if it's an attribute access (e.g., sys.path.insert)
            if node.func.attr == "insert":
                # Check if the object is sys.path
                if isinstance(node.func.value, ast.Attribute):
                    if isinstance(node.func.value.value, ast.Name):
                        if node.func.value.value.id == "sys" and node.func.value.attr == "path":
                            self.errors.append(
                                (
                                    node.lineno,
                                    node.col_offset,
                                    (
                                        "SPI001 sys.path.insert() detected - use proper virtual environment activation instead. "
                                        "Ensure your environment is activated with 'uv run' or 'source .venv/bin/activate'"
                                    ),
                                )
                            )

        # Continue visiting child nodes
        self.generic_visit(node)
