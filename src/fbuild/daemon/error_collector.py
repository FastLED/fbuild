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

    def get_errors(self, severity: Optional[ErrorSeverity] = None) -> list[BuildError]:
        """Get all errors, optionally filtered by severity.

        Args:
            severity: Filter by severity (None = all errors)

        Returns:
            List of build errors
        """
        logging.debug(f"Retrieving errors (severity filter: {severity.value if severity else 'None'})")
        with self.lock:
            if severity:
                filtered = [e for e in self.errors if e.severity == severity]
                logging.debug(f"Filtered {len(filtered)} errors by severity {severity.value} (total: {len(self.errors)})")
                return filtered
            logging.debug(f"Returning all {len(self.errors)} errors")
            return self.errors.copy()

    def get_errors_by_phase(self, phase: str) -> list[BuildError]:
        """Get errors for a specific phase.

        Args:
            phase: Phase to filter by

        Returns:
            List of build errors for the phase
        """
        with self.lock:
            phase_errors = [e for e in self.errors if e.phase == phase]
            logging.debug(f"Found {len(phase_errors)} errors in phase '{phase}' (total: {len(self.errors)})")
            return phase_errors

    def has_fatal_errors(self) -> bool:
        """Check if any fatal errors occurred.

        Returns:
            True if fatal errors exist
        """
        with self.lock:
            has_fatal = any(e.severity == ErrorSeverity.FATAL for e in self.errors)
            fatal_count = sum(1 for e in self.errors if e.severity == ErrorSeverity.FATAL)
            logging.debug(f"Fatal error check result: {has_fatal} ({fatal_count} fatal errors)")
            return has_fatal

    def has_errors(self) -> bool:
        """Check if any errors (non-warning) occurred.

        Returns:
            True if errors exist
        """
        logging.debug("Checking for errors (non-warning)")
        with self.lock:
            has_errs = any(e.severity in (ErrorSeverity.ERROR, ErrorSeverity.FATAL) for e in self.errors)
            error_count = sum(1 for e in self.errors if e.severity in (ErrorSeverity.ERROR, ErrorSeverity.FATAL))
            logging.debug(f"Error check result: {has_errs} ({error_count} errors or fatal)")
            return has_errs

    def has_warnings(self) -> bool:
        """Check if any warnings occurred.

        Returns:
            True if warnings exist
        """
        with self.lock:
            has_warn = any(e.severity == ErrorSeverity.WARNING for e in self.errors)
            warning_count = sum(1 for e in self.errors if e.severity == ErrorSeverity.WARNING)
            logging.debug(f"Warning check result: {has_warn} ({warning_count} warnings)")
            return has_warn

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
        logging.debug(f"Error counts: {counts['total']} total ({counts['fatal']} fatal, {counts['errors']} errors, {counts['warnings']} warnings)")
        return counts

    def format_errors(self, max_errors: Optional[int] = None) -> str:
        """Format all errors as human-readable string.

        Args:
            max_errors: Maximum number of errors to include (None = all)

        Returns:
            Formatted error report
        """
        logging.debug(f"Formatting errors (max_errors: {max_errors})")
        with self.lock:
            if not self.errors:
                return "No errors"

            errors_to_show = self.errors if max_errors is None else self.errors[:max_errors]
            logging.debug(f"Formatting {len(errors_to_show)} errors (total: {len(self.errors)})")
            lines = []

            for err in errors_to_show:
                lines.append(err.format())

            if max_errors and len(self.errors) > max_errors:
                lines.append(f"\n... and {len(self.errors) - max_errors} more errors")

            # Add summary
            counts = self.get_error_count()
            summary = f"\nSummary: {counts['fatal']} fatal, {counts['errors']} errors, {counts['warnings']} warnings"
            lines.append(summary)

            formatted = "\n\n".join(lines)
            logging.debug(f"Error formatting complete: {len(lines)} sections, {len(formatted)} characters")
            return formatted

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

        summary = ", ".join(parts)
        return summary

    def clear(self) -> None:
        """Clear all collected errors."""
        with self.lock:
            error_count = len(self.errors)
            self.errors.clear()

        if error_count > 0:
            logging.info(f"Cleared {error_count} errors")

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
        compilation_errors = self.get_errors_by_phase("compile")
        logging.debug(f"Found {len(compilation_errors)} compilation errors")
        return compilation_errors

    def get_link_errors(self) -> list[BuildError]:
        """Get all link-phase errors.

        Returns:
            List of link errors
        """
        link_errors = self.get_errors_by_phase("link")
        logging.debug(f"Found {len(link_errors)} link errors")
        return link_errors
