"""Flake8 plugin to enforce safe subprocess usage.

This plugin checks that all subprocess calls use the safe wrappers from
fbuild.subprocess_utils to prevent ephemeral shell windows on Windows.

Error Codes:
    SUB001: Direct subprocess.run() call detected - use safe_run() instead
    SUB002: Direct subprocess.Popen() call detected - use safe_popen() instead
    SUB003: Direct subprocess.call() call detected - use safe_run() instead
    SUB004: Direct subprocess.check_call() call detected - use safe_run() instead
    SUB005: Direct subprocess.check_output() call detected - use safe_run() instead

Usage:
    # Run with flake8
    flake8 --select=SUB src/
"""

import ast
from typing import Any, Generator, Tuple, Type


class SubprocessSafetyChecker:
    """Flake8 plugin to check for unsafe subprocess usage."""

    name = "subprocess-safety-checker"
    version = "1.0.0"

    # Error codes and messages
    ERRORS = {
        "SUB001": "SUB001 Direct subprocess.run() call - use safe_run() from fbuild.subprocess_utils",
        "SUB002": "SUB002 Direct subprocess.Popen() call - use safe_popen() from fbuild.subprocess_utils",
        "SUB003": "SUB003 Direct subprocess.call() call - use safe_run() from fbuild.subprocess_utils",
        "SUB004": "SUB004 Direct subprocess.check_call() call - use safe_run() from fbuild.subprocess_utils",
        "SUB005": "SUB005 Direct subprocess.check_output() call - use safe_run() from fbuild.subprocess_utils",
    }

    # Unsafe subprocess methods mapped to error codes
    UNSAFE_METHODS = {
        "run": "SUB001",
        "Popen": "SUB002",
        "call": "SUB003",
        "check_call": "SUB004",
        "check_output": "SUB005",
    }

    # Files/patterns to exclude from checking
    EXCLUDED_PATTERNS = [
        "subprocess_utils.py",  # Implementation file
        "test_subprocess_utils.py",  # Unit tests for subprocess_utils
    ]

    def __init__(self, tree: ast.AST, filename: str = "(none)") -> None:
        """Initialize checker.

        Args:
            tree: AST tree to check
            filename: Name of file being checked
        """
        self._tree = tree
        self._filename = filename

    def run(self) -> Generator[Tuple[int, int, str, Type[Any]], None, None]:
        """Run the checker and yield violations.

        Yields:
            Tuple of (line, column, message, checker_class)
        """
        # Skip excluded files
        for pattern in self.EXCLUDED_PATTERNS:
            if pattern in self._filename:
                return

        visitor = SubprocessCallVisitor()
        visitor.visit(self._tree)

        for line, col, msg in visitor.errors:
            yield (line, col, msg, type(self))


class SubprocessCallVisitor(ast.NodeVisitor):
    """AST visitor to find unsafe subprocess calls."""

    def __init__(self) -> None:
        """Initialize the visitor."""
        self.errors: list[Tuple[int, int, str]] = []

    def visit_Call(self, node: ast.Call) -> None:
        """Visit a Call node and check for subprocess usage.

        Args:
            node: The Call node to check
        """
        # Check for subprocess.method() calls
        if isinstance(node.func, ast.Attribute):
            # Check if the attribute access is on 'subprocess' module
            if isinstance(node.func.value, ast.Name):
                if node.func.value.id == "subprocess":
                    method_name = node.func.attr
                    if method_name in SubprocessSafetyChecker.UNSAFE_METHODS:
                        error_code = SubprocessSafetyChecker.UNSAFE_METHODS[method_name]
                        message = SubprocessSafetyChecker.ERRORS[error_code]
                        self.errors.append((node.lineno, node.col_offset, message))

        # Continue visiting child nodes
        self.generic_visit(node)
