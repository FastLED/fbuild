"""Compile Database - Thread-safe collector for compile_commands.json entries.

This module provides a thread-safe collector that captures compilation commands
during builds and writes them in the JSON Compilation Database Format
(https://clang.llvm.org/docs/JSONCompilationDatabase.html).

Design:
    - Uses 'arguments' array format (not 'command' string) per the spec — clangd prefers this
    - Strips sccache wrapper from commands (clangd doesn't need it)
    - Thread-safe via threading.Lock since compilation is parallel
    - translate_for_clang() returns a new database with translated flags
"""

import json
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from fbuild.platform_configs.board_config_model import BoardConfigModel


@dataclass
class CompileEntry:
    """A single entry in the compilation database.

    Attributes:
        directory: Working directory for the compilation (project_dir)
        file: Absolute path to the source file
        arguments: Full compiler command as a list (no sccache wrapper)
        output: Object file path
    """

    directory: str
    file: str
    arguments: list[str]
    output: str

    def to_dict(self) -> dict[str, object]:
        """Convert to dictionary for JSON serialization."""
        return {
            "directory": self.directory,
            "file": self.file,
            "arguments": self.arguments,
            "output": self.output,
        }


class CompileDatabase:
    """Thread-safe collector for compile_commands.json entries.

    This class collects compilation entries from parallel compilation jobs
    and writes the standard JSON Compilation Database format.

    Usage:
        db = CompileDatabase()
        db.add_entry(directory="/path/to/project", file="/path/to/src.cpp",
                     arguments=["g++", "-c", "src.cpp", "-o", "src.o"], output="src.o")
        db.write(Path("compile_commands.json"))
    """

    def __init__(self) -> None:
        """Initialize empty compile database."""
        self._entries: list[CompileEntry] = []
        self._lock = threading.Lock()

    def add_entry(self, directory: str, file: str, arguments: list[str], output: str) -> None:
        """Add a compilation entry to the database.

        Thread-safe: can be called from multiple compilation threads concurrently.

        Args:
            directory: Working directory (project_dir)
            file: Source file path (absolute)
            arguments: Full compiler command as list (should not include sccache)
            output: Object file path
        """
        entry = CompileEntry(
            directory=directory,
            file=file,
            arguments=list(arguments),
            output=output,
        )
        with self._lock:
            self._entries.append(entry)

    def has_entries(self) -> bool:
        """Check if the database has any entries.

        Returns:
            True if there is at least one entry
        """
        with self._lock:
            return len(self._entries) > 0

    def entry_count(self) -> int:
        """Return the number of entries in the database.

        Returns:
            Number of entries
        """
        with self._lock:
            return len(self._entries)

    def get_entries(self) -> list[CompileEntry]:
        """Get a copy of all entries.

        Returns:
            List of CompileEntry objects
        """
        with self._lock:
            return list(self._entries)

    def to_json(self) -> str:
        """Serialize database to JSON string in standard format.

        Returns:
            JSON string matching the JSON Compilation Database Format spec
        """
        with self._lock:
            entries = [entry.to_dict() for entry in self._entries]
        return json.dumps(entries, indent=2) + "\n"

    def write(self, path: Path) -> None:
        """Write database to a file.

        Args:
            path: Output file path (typically compile_commands.json)
        """
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.to_json(), encoding="utf-8")

    def merge(self, other: "CompileDatabase") -> None:
        """Merge entries from another database into this one.

        Deduplicates by (file, output) pair — if the same source file
        produces the same output, keep the entry from 'other' (latest wins).

        Args:
            other: Another CompileDatabase to merge from
        """
        other_entries = other.get_entries()
        with self._lock:
            # Build index of existing entries by (file, output) for dedup
            existing: dict[tuple[str, str], int] = {}
            for i, entry in enumerate(self._entries):
                existing[(entry.file, entry.output)] = i

            for entry in other_entries:
                key = (entry.file, entry.output)
                if key in existing:
                    # Replace existing entry (latest wins)
                    self._entries[existing[key]] = entry
                else:
                    existing[key] = len(self._entries)
                    self._entries.append(entry)

    def translate_for_clang(self, platform_config: "BoardConfigModel", mcu: str) -> "CompileDatabase":
        """Create a new database with GCC flags translated for clang/clangd.

        Args:
            platform_config: Platform configuration with architecture info
            mcu: MCU identifier (e.g., 'atmega328p', 'esp32c6')

        Returns:
            New CompileDatabase with clang-compatible flags
        """
        from fbuild.build.clang_flag_translator import ClangFlagTranslator

        translated = CompileDatabase()
        architecture = platform_config.architecture

        with self._lock:
            for entry in self._entries:
                new_args = ClangFlagTranslator.translate(entry.arguments, architecture, mcu)
                translated.add_entry(
                    directory=entry.directory,
                    file=entry.file,
                    arguments=new_args,
                    output=entry.output,
                )

        return translated

    @classmethod
    def load(cls, path: Path) -> "CompileDatabase":
        """Load a compile database from a JSON file.

        Args:
            path: Path to compile_commands.json file

        Returns:
            CompileDatabase with loaded entries

        Raises:
            FileNotFoundError: If the file doesn't exist
            json.JSONDecodeError: If the file is not valid JSON
        """
        db = cls()
        if not path.exists():
            return db

        data = json.loads(path.read_text(encoding="utf-8"))
        for entry_dict in data:
            db.add_entry(
                directory=entry_dict["directory"],
                file=entry_dict["file"],
                arguments=entry_dict["arguments"],
                output=entry_dict["output"],
            )
        return db

    @staticmethod
    def strip_sccache(cmd: list[str]) -> list[str]:
        """Remove sccache wrapper from a compiler command.

        If the first element looks like an sccache binary, remove it
        so clangd sees the actual compiler path.

        Args:
            cmd: Full compiler command list

        Returns:
            Command list without sccache prefix
        """
        if not cmd:
            return cmd

        first = cmd[0].lower().replace("\\", "/")
        if "sccache" in first:
            return cmd[1:]
        return list(cmd)
