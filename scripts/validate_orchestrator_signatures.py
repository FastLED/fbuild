#!/usr/bin/env python3
"""
Validate orchestrator method signatures.

This script uses AST parsing to validate that all _build_XXX() methods
in orchestrator files have the correct signature parameters.

All _build_XXX() methods must have:
- project_path parameter (type: str or Path)
- env_name parameter (type: str)
- target parameter (type: str)
- verbose parameter (type: bool)
- clean parameter (type: bool)
- jobs parameter (type: int | None with default = None)

Exit codes:
- 0: All checks passed
- 1: One or more violations found
"""

import ast
import sys
from pathlib import Path
from typing import Any, List, Optional, Set


class MethodSignatureViolation:
    """Represents a violation in a method signature."""

    def __init__(self, file_path: Path, line_number: int, method_name: str, violation: str):
        self.file_path = file_path
        self.line_number = line_number
        self.method_name = method_name
        self.violation = violation

    def __str__(self) -> str:
        return f"{self.file_path.name}:{self.line_number} - {self.method_name}()\n  {self.violation}"


class OrchestratorSignatureValidator:
    """Validates orchestrator method signatures using AST parsing."""

    # Required parameters with their expected types
    # Note: These are internal _build_XXX methods, not the public build() interface
    REQUIRED_PARAMS = {
        'project_dir': {'Path', 'str'},  # Accept either Path or str
        'env_name': {'str'},
        'board_id': {'str'},
        'env_config': {'dict', 'dict[str, Any]', 'dict[(str, Any)]'},  # Accept various dict annotations
        'build_flags': {'List[str]', 'list[str]', 'List', 'list'},
        'lib_deps': {'List[str]', 'list[str]', 'List', 'list'},
        'clean': {'bool'},
        'verbose': {'bool'},
        'jobs': {'int | None', 'int', 'None', 'Optional[int]'},  # Accept union types
    }

    # Parameters that require default values
    REQUIRED_DEFAULTS = {
        'clean': False,
        'verbose': False,
        'jobs': None,  # jobs must default to None
    }

    def __init__(self, root_dir: Path):
        """
        Initialize validator.

        Args:
            root_dir: Root directory of the project
        """
        self.root_dir = root_dir
        self.orchestrator_dir = root_dir / "src" / "fbuild" / "build"
        self.violations: List[MethodSignatureViolation] = []

    def validate_all(self) -> bool:
        """
        Validate all orchestrator files.

        Returns:
            True if all checks passed, False otherwise
        """
        # Map files to their primary _build_XXX method to validate
        # We skip wrapper methods in orchestrator_avr.py
        orchestrator_methods = {
            "orchestrator_esp32.py": "_build_esp32",
            "orchestrator_teensy.py": "_build_teensy",
            "orchestrator_rp2040.py": "_build_rp2040",
            "orchestrator_stm32.py": "_build_stm32",
        }

        print("Validating orchestrator method signatures...")
        print("=" * 80)

        for filename, method_name in orchestrator_methods.items():
            file_path = self.orchestrator_dir / filename
            if not file_path.exists():
                print(f"WARNING: {filename} not found, skipping...")
                continue

            print(f"\nChecking {filename}...")
            self._validate_file(file_path, target_method=method_name)

        return len(self.violations) == 0

    def _validate_file(self, file_path: Path, target_method: Optional[str] = None) -> None:
        """
        Validate all _build_XXX methods in a file.

        Args:
            file_path: Path to the orchestrator file
            target_method: If specified, only validate this specific method
        """
        with open(file_path, 'r', encoding='utf-8') as f:
            source = f.read()

        try:
            tree = ast.parse(source, filename=str(file_path))
        except SyntaxError as e:
            self.violations.append(
                MethodSignatureViolation(
                    file_path, 0, "PARSE_ERROR",
                    f"Failed to parse file: {e}"
                )
            )
            return

        # Find all _build_XXX methods
        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef):
                if node.name.startswith('_build_'):
                    # If target_method specified, only validate that one
                    if target_method and node.name != target_method:
                        continue
                    self._validate_method(file_path, node)

    def _validate_method(self, file_path: Path, method_node: ast.FunctionDef) -> None:
        """
        Validate a single _build_XXX method.

        Args:
            file_path: Path to the file containing the method
            method_node: AST node representing the method
        """
        method_name = method_node.name
        line_number = method_node.lineno

        print(f"  - {method_name}() at line {line_number}")

        # Parse method arguments
        args = method_node.args
        param_info = self._parse_parameters(args)

        # Check for missing parameters
        missing_params = set(self.REQUIRED_PARAMS.keys()) - set(param_info.keys())
        # 'self' is implicit, don't check for it
        if 'self' in missing_params:
            missing_params.remove('self')

        for param_name in missing_params:
            self.violations.append(
                MethodSignatureViolation(
                    file_path, line_number, method_name,
                    f"Missing required parameter: {param_name}"
                )
            )

        # Check parameter types and defaults
        for param_name, (param_type, default_value) in param_info.items():
            if param_name == 'self':
                continue

            if param_name not in self.REQUIRED_PARAMS:
                # This is okay - extra parameters are allowed (like build_flags, lib_deps, etc.)
                continue

            # Check type annotation
            expected_types = self.REQUIRED_PARAMS[param_name]
            if param_type and not self._type_matches(param_type, expected_types):
                self.violations.append(
                    MethodSignatureViolation(
                        file_path, line_number, method_name,
                        f"Parameter '{param_name}' has wrong type: expected one of {expected_types}, got {param_type}"
                    )
                )

            # Check default value
            if param_name in self.REQUIRED_DEFAULTS:
                expected_default = self.REQUIRED_DEFAULTS[param_name]
                if default_value != expected_default:
                    self.violations.append(
                        MethodSignatureViolation(
                            file_path, line_number, method_name,
                            f"Parameter '{param_name}' has wrong default: expected {expected_default}, got {default_value}"
                        )
                    )

    def _parse_parameters(self, args: ast.arguments) -> dict:
        """
        Parse method parameters from AST arguments.

        Args:
            args: AST arguments node

        Returns:
            Dictionary mapping parameter names to (type, default_value) tuples
        """
        param_info = {}

        # Get all positional args
        all_args = args.args

        # Get defaults (aligned to the right)
        defaults = [None] * (len(all_args) - len(args.defaults)) + list(args.defaults)

        for arg, default in zip(all_args, defaults):
            param_name = arg.arg
            param_type = self._extract_type_annotation(arg.annotation)
            default_value = self._extract_default_value(default)
            param_info[param_name] = (param_type, default_value)

        return param_info

    def _extract_type_annotation(self, annotation: Optional[ast.expr]) -> Optional[str]:
        """
        Extract type annotation as a string.

        Args:
            annotation: AST annotation node

        Returns:
            Type annotation as string, or None if not annotated
        """
        if annotation is None:
            return None

        if isinstance(annotation, ast.Name):
            return annotation.id
        elif isinstance(annotation, ast.Constant):
            return str(annotation.value)
        elif isinstance(annotation, ast.BinOp):
            # Handle union types like 'int | None'
            if isinstance(annotation.op, ast.BitOr):
                left = self._extract_type_annotation(annotation.left)
                right = self._extract_type_annotation(annotation.right)
                return f"{left} | {right}"
        elif isinstance(annotation, ast.Subscript):
            # Handle generic types like Optional[str]
            value = self._extract_type_annotation(annotation.value)
            slice_val = self._extract_type_annotation(annotation.slice)
            return f"{value}[{slice_val}]"

        # Fallback: try to unparse the annotation
        try:
            return ast.unparse(annotation)
        except Exception:
            return str(annotation)

    def _extract_default_value(self, default: Optional[ast.expr]) -> Optional[Any]:
        """
        Extract default value from AST node.

        Args:
            default: AST default value node

        Returns:
            Default value, or None if no default
        """
        if default is None:
            return None

        if isinstance(default, ast.Constant):
            return default.value
        elif isinstance(default, ast.Name):
            if default.id == 'None':
                return None
            elif default.id == 'True':
                return True
            elif default.id == 'False':
                return False

        # For complex defaults, return string representation
        try:
            return ast.unparse(default)
        except Exception:
            return str(default)

    def _type_matches(self, actual_type: str, expected_types: Set[str]) -> bool:
        """
        Check if actual type matches any of the expected types.

        Args:
            actual_type: Actual type annotation
            expected_types: Set of acceptable type annotations

        Returns:
            True if type matches
        """
        # Normalize types
        actual_normalized = actual_type.replace(' ', '')

        for expected in expected_types:
            expected_normalized = expected.replace(' ', '')

            # Direct match
            if actual_normalized == expected_normalized:
                return True

            # Check if it's a union type containing the expected type
            if '|' in actual_normalized:
                parts = actual_normalized.split('|')
                if expected_normalized in parts:
                    return True

            # Check Optional[X] as X | None
            if actual_normalized.startswith('Optional['):
                inner = actual_normalized[9:-1]  # Extract type from Optional[...]
                if inner == expected_normalized:
                    return True

        return False

    def print_violations(self) -> None:
        """Print all violations found."""
        if not self.violations:
            print("\n" + "=" * 80)
            print("✓ All orchestrator method signatures are valid!")
            print("=" * 80)
            return

        print("\n" + "=" * 80)
        print(f"✗ Found {len(self.violations)} violation(s):")
        print("=" * 80)

        for violation in self.violations:
            print(f"\n{violation}")

        print("\n" + "=" * 80)


def print_help() -> None:
    """Print help text."""
    help_text = """
validate_orchestrator_signatures.py - Validate orchestrator method signatures

USAGE:
    python scripts/validate_orchestrator_signatures.py [OPTIONS]

DESCRIPTION:
    This script uses AST parsing to validate that all _build_XXX() methods
    in orchestrator files have the correct signature parameters.

    All _build_XXX() methods must have:
    - project_dir parameter (type: Path or str)
    - env_name parameter (type: str)
    - board_id parameter (type: str)
    - env_config parameter (type: dict)
    - build_flags parameter (type: List[str])
    - lib_deps parameter (type: List[str])
    - clean parameter (type: bool, default: False)
    - verbose parameter (type: bool, default: False)
    - jobs parameter (type: int | None, default: None)

    The script validates the following orchestrator files:
    - orchestrator_esp32.py (_build_esp32 method)
    - orchestrator_teensy.py (_build_teensy method)
    - orchestrator_rp2040.py (_build_rp2040 method)
    - orchestrator_stm32.py (_build_stm32 method)

OPTIONS:
    -h, --help      Show this help message and exit

EXIT CODES:
    0   All checks passed
    1   One or more violations found

EXAMPLES:
    # Run validation
    python scripts/validate_orchestrator_signatures.py

    # Show help
    python scripts/validate_orchestrator_signatures.py --help
"""
    print(help_text)


def main() -> int:
    """
    Main entry point.

    Returns:
        Exit code (0 = success, 1 = violations found)
    """
    # Check for help flag
    if len(sys.argv) > 1 and sys.argv[1] in ('-h', '--help'):
        print_help()
        return 0

    # Determine project root (script is in scripts/, root is parent)
    script_dir = Path(__file__).parent
    root_dir = script_dir.parent

    validator = OrchestratorSignatureValidator(root_dir)

    # Run validation
    success = validator.validate_all()

    # Print results
    validator.print_violations()

    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
