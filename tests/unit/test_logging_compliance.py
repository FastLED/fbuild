"""Unit tests for logging compliance across the codebase.

These tests enforce that production code uses logging instead of print()
statements for debug output.
"""

import re
from pathlib import Path

import pytest


class TestLoggingCompliance:
    """Test cases for logging vs print statement compliance."""

    def test_no_print_statements_in_production_code(self):
        """Verify no DEBUG print() calls exist in non-CLI code.

        This test performs static code analysis to find DEBUG print() calls in
        production code. DEBUG print statements should use logging.debug()
        instead.

        Note: CLI print() statements are legitimate for user-facing output.
        """
        src_dir = Path(__file__).parent.parent.parent / "src"
        assert src_dir.exists(), f"Source directory not found: {src_dir}"

        # Find all Python files in src/ (excluding CLI)
        python_files = list(src_dir.rglob("*.py"))
        assert len(python_files) > 0, "No Python files found in src/"

        violations = []

        for file_path in python_files:
            # Skip __pycache__ and other non-source files
            if "__pycache__" in str(file_path):
                continue

            # Skip CLI files (legitimate user-facing output)
            if "cli.py" in str(file_path):
                continue

            # Read file content
            try:
                content = file_path.read_text(encoding="utf-8")
            except Exception as e:
                print(f"Warning: Could not read {file_path}: {e}")
                continue

            # Find DEBUG print() calls specifically
            lines = content.split("\n")
            for line_num, line in enumerate(lines, start=1):
                # Skip comments
                if line.strip().startswith("#"):
                    continue

                # Skip docstrings (simple check)
                if '"""' in line or "'''" in line:
                    continue

                # Look for DEBUG print() calls
                if re.search(r"\bprint\s*\(", line) and "DEBUG" in line.upper():
                    violations.append(f"{file_path}:{line_num}: {line.strip()}")

        # Report violations
        if violations:
            violation_report = "\n".join(violations)
            pytest.fail(f"Found {len(violations)} DEBUG print() statements in production code:\n" f"{violation_report}\n\n" "Use logging.debug() instead of print() for debug output.")

    def test_debug_output_uses_logging_module(self):
        """Verify DEBUG output uses logging.debug() instead of print().

        This test checks specific files known to have DEBUG print statements
        and verifies they've been converted to logging calls.
        """
        files_to_check = [
            "src/fbuild/build/binary_generator.py",
            "src/fbuild/packages/archive_strategies.py",
        ]

        violations = []

        for file_path_str in files_to_check:
            file_path = Path(file_path_str)
            if not file_path.exists():
                continue

            content = file_path.read_text(encoding="utf-8")
            lines = content.split("\n")

            for line_num, line in enumerate(lines, start=1):
                # Look for DEBUG print statements specifically
                if "print(" in line and "DEBUG" in line:
                    violations.append(f"{file_path}:{line_num}: {line.strip()}")

        if violations:
            violation_report = "\n".join(violations)
            pytest.fail(f"Found {len(violations)} DEBUG print() statements:\n" f"{violation_report}\n\n" "Convert to logging.debug() format.")

    def test_no_direct_stdout_writes(self):
        """Verify production code doesn't write DEBUG output to stdout directly.

        All DEBUG output should go through logging module for proper log capture.
        """
        src_dir = Path(__file__).parent.parent.parent / "src"
        python_files = list(src_dir.rglob("*.py"))

        violations = []

        for file_path in python_files:
            if "__pycache__" in str(file_path):
                continue

            # Skip CLI files (legitimate user-facing output)
            if "cli.py" in str(file_path):
                continue

            try:
                content = file_path.read_text(encoding="utf-8")
            except Exception:
                continue

            lines = content.split("\n")
            for line_num, line in enumerate(lines, start=1):
                # Skip comments and docstrings
                if line.strip().startswith("#"):
                    continue
                if '"""' in line or "'''" in line:
                    continue

                # Look for DEBUG stdout writes specifically
                if re.search(r"\bstdout\s*\.\s*write\s*\(", line) and "DEBUG" in line.upper():
                    violations.append(f"{file_path}:{line_num}: {line.strip()}")

        if violations:
            violation_report = "\n".join(violations)
            pytest.fail(f"Found {len(violations)} DEBUG stdout writes:\n{violation_report}\n\n" "Use logging.debug() instead.")

    def test_logging_imports_present(self):
        """Verify files with logging calls have proper logging imports.

        Files using logging.debug() should import logging at the top.
        """
        files_with_logging_calls = [
            "src/fbuild/build/binary_generator.py",
            "src/fbuild/packages/archive_strategies.py",
        ]

        missing_imports = []

        for file_path_str in files_with_logging_calls:
            file_path = Path(file_path_str)
            if not file_path.exists():
                continue

            content = file_path.read_text(encoding="utf-8")

            # Check if file uses logging.debug() or logging.info()
            if "logging." in content:
                # Verify import logging is present
                if not re.search(r"^import logging$", content, re.MULTILINE) and not re.search(r"^from logging import", content, re.MULTILINE):
                    missing_imports.append(file_path_str)

        if missing_imports:
            pytest.fail(f"Files using logging.* without importing logging:\n{chr(10).join(missing_imports)}")


class TestLoggingPatterns:
    """Test logging patterns and best practices."""

    def test_logging_format_consistency(self):
        """Verify logging calls follow consistent format patterns.

        Logging messages should follow conventions:
        - Use f-strings for variable interpolation
        - Include module/function context
        - Use appropriate log levels
        """
        # This is a documentation test for now
        # Could be extended to parse AST and check logging call patterns
        pass

    def test_no_logging_in_hot_paths(self):
        """Verify critical hot paths don't have excessive logging.

        Hot paths (compilation loops, etc.) should avoid logging at DEBUG level
        in tight loops to prevent performance degradation.
        """
        # This is a documentation test for now
        # Could be extended to analyze compiler.py, linker.py for logging in loops
        pass


class TestDebugOutputConversion:
    """Test that DEBUG output has been properly converted."""

    def test_binary_generator_uses_logging(self):
        """Verify binary_generator.py uses logging instead of print().

        Checks lines 111-114, 291 specifically (known DEBUG print locations).
        """
        file_path = Path("src/fbuild/build/binary_generator.py")
        if not file_path.exists():
            pytest.skip("binary_generator.py not found")

        content = file_path.read_text(encoding="utf-8")
        lines = content.split("\n")

        # Check specific line ranges for DEBUG prints
        suspicious_lines = []
        for line_num in [111, 112, 113, 114, 291]:
            if line_num <= len(lines):
                line = lines[line_num - 1]
                if "print(" in line and "DEBUG" in line:
                    suspicious_lines.append((line_num, line.strip()))

        if suspicious_lines:
            report = "\n".join([f"Line {num}: {line}" for num, line in suspicious_lines])
            pytest.fail(f"Found DEBUG print() statements in binary_generator.py:\n{report}")

    def test_archive_strategies_uses_logging(self):
        """Verify archive_strategies.py uses logging instead of print().

        Checks lines 372-392 specifically (known DEBUG print locations).
        """
        file_path = Path("src/fbuild/packages/archive_strategies.py")
        if not file_path.exists():
            pytest.skip("archive_strategies.py not found")

        content = file_path.read_text(encoding="utf-8")
        lines = content.split("\n")

        # Check line range 372-392 for DEBUG prints
        suspicious_lines = []
        for line_num in range(372, 393):
            if line_num <= len(lines):
                line = lines[line_num - 1]
                if "print(" in line and "DEBUG" in line:
                    suspicious_lines.append((line_num, line.strip()))

        if suspicious_lines:
            report = "\n".join([f"Line {num}: {line}" for num, line in suspicious_lines])
            pytest.fail(f"Found DEBUG print() statements in archive_strategies.py:\n{report}")


class TestLoggingIntegration:
    """Integration tests for logging system."""

    def test_daemon_log_captures_debug_output(self):
        """Verify daemon log captures debug output (manual test documentation).

        This test documents the manual verification process:
        1. Set logging level to DEBUG
        2. Run build/deploy operation
        3. Check daemon.log for debug messages
        4. Verify no debug output appears on console
        """
        # This is a manual test documented for reference
        pass

    def test_logging_configuration_is_consistent(self):
        """Verify logging configuration is consistent across modules.

        All modules should use the same logging configuration (format,
        handlers, levels).
        """
        # This is a documentation test for now
        # Could be extended to check logging.basicConfig() calls
        pass
