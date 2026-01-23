"""Flake8 plugin to validate message serialization completeness.

This plugin ensures that all dataclass message types properly serialize and
deserialize all their fields in to_dict() and from_dict() methods.

Error Codes:
    MSC001: Dataclass field not included in from_dict() method
    MSC002: Dataclass field not serialized in to_dict() method

Usage:
    flake8 src/fbuild/daemon/messages.py --select=MSC
"""

import ast
from typing import Any, Generator, Tuple, Type, Set


class MessageSerializationChecker:
    """Flake8 plugin to check message serialization completeness."""

    name = "message-serialization-checker"
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
        visitor = MessageVisitor()
        visitor.visit(self._tree)

        for line, col, msg in visitor.errors:
            yield (line, col, msg, type(self))


class MessageVisitor(ast.NodeVisitor):
    """AST visitor to check message serialization."""

    def __init__(self) -> None:
        """Initialize the visitor."""
        self.errors: list[Tuple[int, int, str]] = []
        self._current_class: str | None = None
        self._dataclass_fields: Set[str] = set()
        self._has_to_dict: bool = False
        self._has_from_dict: bool = False
        self._to_dict_method: ast.FunctionDef | None = None
        self._from_dict_method: ast.FunctionDef | None = None

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        """Visit a class definition and check if it's a dataclass with serialization methods.

        Args:
            node: The ClassDef node to check
        """
        # Reset state
        self._current_class = node.name
        self._dataclass_fields = set()
        self._has_to_dict = False
        self._has_from_dict = False
        self._to_dict_method = None
        self._from_dict_method = None

        # Check if this is a dataclass
        is_dataclass = False
        for decorator in node.decorator_list:
            if isinstance(decorator, ast.Name) and decorator.id == "dataclass":
                is_dataclass = True
                break
            elif isinstance(decorator, ast.Call):
                if isinstance(decorator.func, ast.Name) and decorator.func.id == "dataclass":
                    is_dataclass = True
                    break

        if not is_dataclass:
            self.generic_visit(node)
            return

        # Collect dataclass fields
        for item in node.body:
            if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name):
                # This is a dataclass field
                self._dataclass_fields.add(item.target.id)

        # Find to_dict and from_dict methods
        for item in node.body:
            if isinstance(item, ast.FunctionDef):
                if item.name == "to_dict":
                    self._has_to_dict = True
                    self._to_dict_method = item
                elif item.name == "from_dict":
                    self._has_from_dict = True
                    self._from_dict_method = item

        # Only check classes that have both serialization methods
        if self._has_to_dict and self._has_from_dict:
            self._check_serialization()

        # Continue visiting child nodes
        self.generic_visit(node)

    def _check_serialization(self) -> None:
        """Check that all fields are properly serialized and deserialized."""
        if not self._dataclass_fields:
            return

        # Check if using helper functions (serialize_dataclass/deserialize_dataclass)
        if self._uses_helper_functions():
            # Skip validation for classes using helper functions
            return

        # Check to_dict() method
        if self._to_dict_method:
            serialized_fields = self._extract_serialized_fields(self._to_dict_method)
            missing_in_to_dict = self._dataclass_fields - serialized_fields

            for field in missing_in_to_dict:
                self.errors.append(
                    (
                        self._to_dict_method.lineno,
                        self._to_dict_method.col_offset,
                        f"MSC002 Field '{field}' in dataclass {self._current_class} not serialized in to_dict() method",
                    )
                )

        # Check from_dict() method
        if self._from_dict_method:
            deserialized_fields = self._extract_deserialized_fields(self._from_dict_method)

            # Filter out fields with default values (they're optional in from_dict)
            required_fields = self._get_required_fields()
            missing_in_from_dict = required_fields - deserialized_fields

            for field in missing_in_from_dict:
                self.errors.append(
                    (
                        self._from_dict_method.lineno,
                        self._from_dict_method.col_offset,
                        f"MSC001 Required field '{field}' in dataclass {self._current_class} not included in from_dict() method",
                    )
                )

    def _uses_helper_functions(self) -> bool:
        """Check if the class uses serialize_dataclass/deserialize_dataclass helpers.

        Returns:
            True if helper functions are used
        """
        if not self._to_dict_method or not self._from_dict_method:
            return False

        uses_serialize = False
        uses_deserialize = False

        # Check to_dict for serialize_dataclass
        for node in ast.walk(self._to_dict_method):
            if isinstance(node, ast.Call):
                if isinstance(node.func, ast.Name) and node.func.id == "serialize_dataclass":
                    uses_serialize = True
                    break

        # Check from_dict for deserialize_dataclass
        for node in ast.walk(self._from_dict_method):
            if isinstance(node, ast.Call):
                if isinstance(node.func, ast.Name) and node.func.id == "deserialize_dataclass":
                    uses_deserialize = True
                    break

        return uses_serialize and uses_deserialize

    def _get_required_fields(self) -> Set[str]:
        """Get fields that don't have default values (required in from_dict).

        Returns:
            Set of required field names
        """
        # For now, assume all fields without defaults are required
        # This is a simplified check - in practice we'd need to parse field() calls
        # and check for default_factory
        return self._dataclass_fields

    def _extract_serialized_fields(self, method: ast.FunctionDef) -> Set[str]:
        """Extract fields that are serialized in to_dict().

        Args:
            method: The to_dict() method node

        Returns:
            Set of field names that are serialized
        """
        serialized = set()

        # Look for patterns like:
        # - return asdict(self)
        # - return {"field": self.field, ...}
        # - result["field"] = self.field

        for node in ast.walk(method):
            # Check for asdict(self) - this serializes all fields
            if isinstance(node, ast.Call):
                if isinstance(node.func, ast.Name) and node.func.id == "asdict":
                    # asdict serializes all fields
                    return self._dataclass_fields

            # Check for dictionary literals with field assignments
            if isinstance(node, ast.Dict):
                for key in node.keys:
                    if isinstance(key, ast.Constant) and isinstance(key.value, str):
                        serialized.add(key.value)

            # Check for dictionary subscript assignments: result["field"] = ...
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    if isinstance(target, ast.Subscript):
                        if isinstance(target.slice, ast.Constant):
                            if isinstance(target.slice.value, str):
                                serialized.add(target.slice.value)

        return serialized

    def _extract_deserialized_fields(self, method: ast.FunctionDef) -> Set[str]:
        """Extract fields that are deserialized in from_dict().

        Args:
            method: The from_dict() method node

        Returns:
            Set of field names that are deserialized
        """
        deserialized = set()

        # Look for patterns like:
        # - cls(**data)
        # - cls(field=data["field"], ...)
        # - data.get("field")
        # - data["field"]

        for node in ast.walk(method):
            # Check for cls(**data) - this deserializes all fields
            if isinstance(node, ast.Call):
                for keyword in node.keywords:
                    if keyword.arg is None:  # **kwargs
                        # This likely unpacks all fields
                        return self._dataclass_fields

            # Check for keyword arguments in constructor call
            if isinstance(node, ast.Call):
                for keyword in node.keywords:
                    if keyword.arg:
                        deserialized.add(keyword.arg)

            # Check for data["field"] or data.get("field")
            if isinstance(node, ast.Subscript):
                if isinstance(node.slice, ast.Constant):
                    if isinstance(node.slice.value, str):
                        deserialized.add(node.slice.value)

            if isinstance(node, ast.Call):
                if isinstance(node.func, ast.Attribute):
                    if node.func.attr == "get" and node.args:
                        if isinstance(node.args[0], ast.Constant):
                            if isinstance(node.args[0].value, str):
                                deserialized.add(node.args[0].value)

        return deserialized
