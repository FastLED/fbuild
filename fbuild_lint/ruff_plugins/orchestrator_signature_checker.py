"""Flake8 plugin to validate orchestrator build() method signatures.

This plugin ensures that all platform-specific orchestrators implement the correct
build() method signature with all required parameters and type annotations.

Error Codes:
    OSC001: Missing required parameter in build() method
    OSC002: Incorrect parameter type annotation in build() method
    OSC003: Missing return type annotation in build() method

Usage:
    flake8 src --select=OSC
"""

import ast
from typing import Any, Generator, Tuple, Type


class OrchestratorSignatureChecker:
    """Flake8 plugin to check orchestrator build() method signatures."""

    name = "orchestrator-signature-checker"
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
        visitor = OrchestratorVisitor()
        visitor.visit(self._tree)

        for line, col, msg in visitor.errors:
            yield (line, col, msg, type(self))


class OrchestratorVisitor(ast.NodeVisitor):
    """AST visitor to check orchestrator build() method signatures."""

    # Required parameters for build() method
    REQUIRED_PARAMS = {
        "project_dir": "Path",
        "env_name": "Optional[str]",
        "clean": "bool",
        "verbose": "Optional[bool]",
        "jobs": "int | None",
    }

    # Expected return type
    EXPECTED_RETURN_TYPE = "BuildResult"

    def __init__(self) -> None:
        """Initialize the visitor."""
        self.errors: list[Tuple[int, int, str]] = []
        self._current_class: str | None = None

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        """Visit a class definition and check if it inherits from PlatformOrchestrator.

        Args:
            node: The ClassDef node to check
        """
        # Check if this class inherits from IBuildOrchestrator or has "Orchestrator" in the name
        is_orchestrator = False

        for base in node.bases:
            if isinstance(base, ast.Name):
                if base.id == "IBuildOrchestrator":
                    is_orchestrator = True
                    break
            elif isinstance(base, ast.Attribute):
                if base.attr == "IBuildOrchestrator":
                    is_orchestrator = True
                    break

        # Also check class name pattern
        if node.name.startswith("BuildOrchestrator"):
            is_orchestrator = True

        if is_orchestrator:
            self._current_class = node.name
            # Look for build() method
            for item in node.body:
                if isinstance(item, ast.FunctionDef) and item.name == "build":
                    self._check_build_method(item)
            self._current_class = None

        # Continue visiting child nodes
        self.generic_visit(node)

    def _check_build_method(self, node: ast.FunctionDef) -> None:
        """Check if build() method has correct signature.

        Args:
            node: The FunctionDef node for the build() method
        """
        # Check return type annotation
        if node.returns is None:
            self.errors.append(
                (
                    node.lineno,
                    node.col_offset,
                    f"OSC003 build() method in {self._current_class} missing return type annotation (expected: BuildResult)",
                )
            )
        else:
            # Check if return type is BuildResult
            return_type = self._extract_type_annotation(node.returns)
            if return_type != self.EXPECTED_RETURN_TYPE:
                self.errors.append(
                    (
                        node.lineno,
                        node.col_offset,
                        f"OSC003 build() method in {self._current_class} has incorrect return type '{return_type}' (expected: BuildResult)",
                    )
                )

        # Get all parameters (skip 'self')
        params = {arg.arg: arg for arg in node.args.args if arg.arg != "self"}

        # Check for missing required parameters
        for param_name, expected_type in self.REQUIRED_PARAMS.items():
            if param_name not in params:
                self.errors.append(
                    (
                        node.lineno,
                        node.col_offset,
                        f"OSC001 build() method in {self._current_class} missing required parameter '{param_name}: {expected_type}'",
                    )
                )
            else:
                # Check type annotation
                param_node = params[param_name]
                if param_node.annotation is None:
                    self.errors.append(
                        (
                            node.lineno,
                            node.col_offset,
                            f"OSC002 Parameter '{param_name}' in build() method missing type annotation (expected: {expected_type})",
                        )
                    )
                else:
                    actual_type = self._extract_type_annotation(param_node.annotation)
                    # Normalize type representations
                    if not self._types_match(actual_type, expected_type):
                        self.errors.append(
                            (
                                node.lineno,
                                node.col_offset,
                                f"OSC002 Parameter '{param_name}' has incorrect type annotation '{actual_type}' (expected: {expected_type})",
                            )
                        )

    def _extract_type_annotation(self, annotation: ast.expr) -> str:
        """Extract type annotation as a string.

        Args:
            annotation: The annotation AST node

        Returns:
            String representation of the type annotation
        """
        if isinstance(annotation, ast.Name):
            return annotation.id
        elif isinstance(annotation, ast.Constant):
            return str(annotation.value)
        elif isinstance(annotation, ast.Subscript):
            # Handle Optional[T], List[T], etc.
            value = self._extract_type_annotation(annotation.value)
            slice_value = self._extract_type_annotation(annotation.slice)
            return f"{value}[{slice_value}]"
        elif isinstance(annotation, ast.BinOp):
            # Handle union types (int | None)
            if isinstance(annotation.op, ast.BitOr):
                left = self._extract_type_annotation(annotation.left)
                right = self._extract_type_annotation(annotation.right)
                return f"{left} | {right}"
        elif isinstance(annotation, ast.Attribute):
            # Handle module.Type
            value = self._extract_type_annotation(annotation.value)
            return f"{value}.{annotation.attr}"
        return "Unknown"

    def _types_match(self, actual: str, expected: str) -> bool:
        """Check if two type annotations match.

        Handles different representations of the same type:
        - Optional[str] vs str | None
        - Optional[bool] vs bool | None

        Args:
            actual: The actual type annotation
            expected: The expected type annotation

        Returns:
            True if the types match
        """
        # Direct match
        if actual == expected:
            return True

        # Normalize Optional[T] to T | None
        def normalize(type_str: str) -> str:
            if type_str.startswith("Optional[") and type_str.endswith("]"):
                inner = type_str[9:-1]
                return f"{inner} | None"
            return type_str

        actual_normalized = normalize(actual)
        expected_normalized = normalize(expected)

        return actual_normalized == expected_normalized
