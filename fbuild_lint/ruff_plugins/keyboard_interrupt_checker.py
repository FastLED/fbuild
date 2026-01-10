"""Flake8 plugin to check for proper KeyboardInterrupt handling.

This plugin ensures that try-except blocks that catch broad exceptions
(like Exception or BaseException) also properly handle KeyboardInterrupt.

Error Codes:
    KBI001: Try-except catches Exception/BaseException without KeyboardInterrupt handler
"""

import ast
from typing import Any, Generator, Tuple, Type


class KeyboardInterruptChecker:
    """Flake8 plugin to check for proper KeyboardInterrupt handling."""

    name = "keyboard-interrupt-checker"
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
        visitor = TryExceptVisitor()
        visitor.visit(self._tree)

        for line, col, msg in visitor.errors:
            yield (line, col, msg, type(self))


class TryExceptVisitor(ast.NodeVisitor):
    """AST visitor to check try-except blocks for KeyboardInterrupt handling."""

    def __init__(self) -> None:
        """Initialize the visitor."""
        self.errors: list[Tuple[int, int, str]] = []

    def visit_Try(self, node: ast.Try) -> None:
        """Visit a Try node and check exception handlers.

        Args:
            node: The Try node to check
        """
        # Check if any handler catches Exception or BaseException
        catches_broad_exception = False
        has_keyboard_interrupt_handler = False

        for handler in node.handlers:
            if handler.type is None:
                # bare except: catches everything
                catches_broad_exception = True
            elif isinstance(handler.type, ast.Name):
                if handler.type.id in ("Exception", "BaseException"):
                    catches_broad_exception = True
                elif handler.type.id == "KeyboardInterrupt":
                    has_keyboard_interrupt_handler = True
            elif isinstance(handler.type, ast.Tuple):
                # Check if tuple contains Exception or BaseException
                for exc_type in handler.type.elts:
                    if isinstance(exc_type, ast.Name):
                        if exc_type.id in ("Exception", "BaseException"):
                            catches_broad_exception = True
                        elif exc_type.id == "KeyboardInterrupt":
                            has_keyboard_interrupt_handler = True

        # If we catch broad exceptions without KeyboardInterrupt handler, that's an error
        if catches_broad_exception and not has_keyboard_interrupt_handler:
            self.errors.append(
                (
                    node.lineno,
                    node.col_offset,
                    (
                        "KBI001 Try-except catches Exception/BaseException without KeyboardInterrupt handler. "
                        "Add: except KeyboardInterrupt as ke: handle_keyboard_interrupt_properly(ke)"
                    ),
                )
            )

        # Continue visiting child nodes
        self.generic_visit(node)
