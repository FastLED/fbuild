"""
Error Collector - Structured error collection for async operations.

This module provides error collection and aggregation for asynchronous build
operations, replacing simple exception handling with structured error tracking.
"""

import logging
import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


class ErrorSeverity(Enum):
    """Severity level of a build error."""

    WARNING = "warning"
    ERROR = "error"
    FATAL = "fatal"


@dataclass
class BuildError:
    """Single build error."""

    severity: ErrorSeverity
    phase: str  # "download", "compile", "link", "upload"
    file_path: Optional[str]
    error_message: str
    stderr: Optional[str] = None
    stdout: Optional[str] = None
    timestamp: float = field(default_factory=time.time)

    def format(self) -> str:
        """Format error as human-readable string.

        Returns:
            Formatted error message
        """
        lines = [f"[{self.severity.value.upper()}] {self.phase}: {self.error_message}"]

        if self.file_path:
            lines.append(f"  File: {self.file_path}")

        if self.stderr:
            # Truncate stderr to reasonable length
            stderr_preview = self.stderr[:500]
            if len(self.stderr) > 500:
                stderr_preview += "... (truncated)"
            lines.append(f"  stderr: {stderr_preview}")

        return "\n".join(lines)


class ErrorCollector:
    """Collects errors during async build operations."""

    def __init__(self, max_errors: int = 100):
        """Initialize error collector.

        Args:
            max_errors: Maximum number of errors to collect
        """
        self.errors: list[BuildError] = []
        self.lock = threading.Lock()
        self.max_errors = max_errors

        logging.debug(f"ErrorCollector initialized (max_errors={max_errors})")

    def add_error(self, error: BuildError) -> None:
        """Add error to collection.

        Args:
            error: Build error to add
        """
        with self.lock:
            if len(self.errors) >= self.max_errors:
                logging.warning(f"ErrorCollector full ({self.max_errors} errors), dropping oldest")
                self.errors.pop(0)

            self.errors.append(error)

        logging.debug(f"Added {error.severity.value} error in phase {error.phase}: {error.error_message}")

    def get_errors(self, severity: Optional[ErrorSeverity] = None) -> list[BuildError]:
        """Get all errors, optionally filtered by severity.

        Args:
            severity: Filter by severity (None = all errors)

        Returns:
            List of build errors
        """
        with self.lock:
            if severity:
                return [e for e in self.errors if e.severity == severity]
            return self.errors.copy()

    def get_errors_by_phase(self, phase: str) -> list[BuildError]:
        """Get errors for a specific phase.

        Args:
            phase: Phase to filter by

        Returns:
            List of build errors for the phase
        """
        with self.lock:
            return [e for e in self.errors if e.phase == phase]

    def has_fatal_errors(self) -> bool:
        """Check if any fatal errors occurred.

        Returns:
            True if fatal errors exist
        """
        with self.lock:
            return any(e.severity == ErrorSeverity.FATAL for e in self.errors)

    def has_errors(self) -> bool:
        """Check if any errors (non-warning) occurred.

        Returns:
            True if errors exist
        """
        with self.lock:
            return any(e.severity in (ErrorSeverity.ERROR, ErrorSeverity.FATAL) for e in self.errors)

    def has_warnings(self) -> bool:
        """Check if any warnings occurred.

        Returns:
            True if warnings exist
        """
        with self.lock:
            return any(e.severity == ErrorSeverity.WARNING for e in self.errors)

    def get_error_count(self) -> dict[str, int]:
        """Get count of errors by severity.

        Returns:
            Dictionary with counts by severity
        """
        with self.lock:
            counts = {
                "warnings": sum(1 for e in self.errors if e.severity == ErrorSeverity.WARNING),
                "errors": sum(1 for e in self.errors if e.severity == ErrorSeverity.ERROR),
                "fatal": sum(1 for e in self.errors if e.severity == ErrorSeverity.FATAL),
                "total": len(self.errors),
            }
        return counts

    def format_errors(self, max_errors: Optional[int] = None) -> str:
        """Format all errors as human-readable string.

        Args:
            max_errors: Maximum number of errors to include (None = all)

        Returns:
            Formatted error report
        """
        with self.lock:
            if not self.errors:
                return "No errors"

            errors_to_show = self.errors if max_errors is None else self.errors[:max_errors]
            lines = []

            for err in errors_to_show:
                lines.append(err.format())

            if max_errors and len(self.errors) > max_errors:
                lines.append(f"\n... and {len(self.errors) - max_errors} more errors")

            # Add summary
            counts = self.get_error_count()
            summary = f"\nSummary: {counts['fatal']} fatal, {counts['errors']} errors, {counts['warnings']} warnings"
            lines.append(summary)

            return "\n\n".join(lines)

    def format_summary(self) -> str:
        """Format a brief summary of errors.

        Returns:
            Brief error summary
        """
        counts = self.get_error_count()
        if counts["total"] == 0:
            return "No errors"

        parts = []
        if counts["fatal"] > 0:
            parts.append(f"{counts['fatal']} fatal")
        if counts["errors"] > 0:
            parts.append(f"{counts['errors']} errors")
        if counts["warnings"] > 0:
            parts.append(f"{counts['warnings']} warnings")

        return ", ".join(parts)

    def clear(self) -> None:
        """Clear all collected errors."""
        with self.lock:
            error_count = len(self.errors)
            self.errors.clear()

        if error_count > 0:
            logging.debug(f"Cleared {error_count} errors from ErrorCollector")

    def get_first_fatal_error(self) -> Optional[BuildError]:
        """Get the first fatal error encountered.

        Returns:
            First fatal error or None
        """
        with self.lock:
            for error in self.errors:
                if error.severity == ErrorSeverity.FATAL:
                    return error
        return None

    def get_compilation_errors(self) -> list[BuildError]:
        """Get all compilation-phase errors.

        Returns:
            List of compilation errors
        """
        return self.get_errors_by_phase("compile")

    def get_link_errors(self) -> list[BuildError]:
        """Get all link-phase errors.

        Returns:
            List of link errors
        """
        return self.get_errors_by_phase("link")
