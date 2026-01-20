"""Build state tracking for cache invalidation.

This module tracks build configuration state to detect when builds need to be
invalidated due to changes in:
- fbuild version (automatic invalidation on version updates)
- platformio.ini configuration
- Framework versions
- Platform versions
- Toolchain versions
- Library dependencies
- Source files (.ino, .cpp, .c, .h)

Design:
    - Stores build state metadata in .fbuild/build/{env_name}/build_state.json
    - Compares current state with saved state to detect changes
    - Provides clear reasons when invalidation is needed
    - Version tracking ensures caches are invalidated when fbuild is updated
"""

import hashlib
import json
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from fbuild import __version__ as FBUILD_VERSION  # noqa: E402


class BuildStateError(Exception):
    """Raised when build state operations fail."""
    pass


class BuildState:
    """Represents the current state of a build configuration."""

    def __init__(
        self,
        platformio_ini_hash: str,
        platform: str,
        board: str,
        framework: str,
        toolchain_version: Optional[str] = None,
        framework_version: Optional[str] = None,
        platform_version: Optional[str] = None,
        build_flags: Optional[List[str]] = None,
        lib_deps: Optional[List[str]] = None,
        source_files_hash: Optional[str] = None,
        fbuild_version: Optional[str] = None,
    ):
        """Initialize build state.

        Args:
            platformio_ini_hash: SHA256 hash of platformio.ini content
            platform: Platform name (e.g., 'atmelavr', 'espressif32')
            board: Board ID (e.g., 'uno', 'esp32dev')
            framework: Framework name (e.g., 'arduino')
            toolchain_version: Version of toolchain (e.g., '7.3.0-atmel3.6.1-arduino7')
            framework_version: Version of framework (e.g., '1.8.6')
            platform_version: Version of platform package
            build_flags: List of build flags from platformio.ini
            lib_deps: List of library dependencies from platformio.ini
            source_files_hash: Combined hash of all source files (.ino, .cpp, .c, .h)
            fbuild_version: Version of fbuild (for automatic cache invalidation on updates)
        """
        self.platformio_ini_hash = platformio_ini_hash
        self.platform = platform
        self.board = board
        self.framework = framework
        self.toolchain_version = toolchain_version
        self.framework_version = framework_version
        self.platform_version = platform_version
        self.build_flags = build_flags or []
        self.lib_deps = lib_deps or []
        self.source_files_hash = source_files_hash
        self.fbuild_version = fbuild_version or FBUILD_VERSION

    def to_dict(self) -> Dict:
        """Convert to dictionary for JSON serialization."""
        return {
            "platformio_ini_hash": self.platformio_ini_hash,
            "platform": self.platform,
            "board": self.board,
            "framework": self.framework,
            "toolchain_version": self.toolchain_version,
            "framework_version": self.framework_version,
            "platform_version": self.platform_version,
            "build_flags": self.build_flags,
            "lib_deps": self.lib_deps,
            "source_files_hash": self.source_files_hash,
            "fbuild_version": self.fbuild_version,
        }

    @classmethod
    def from_dict(cls, data: Dict) -> "BuildState":
        """Create from dictionary."""
        return cls(
            platformio_ini_hash=data["platformio_ini_hash"],
            platform=data["platform"],
            board=data["board"],
            framework=data["framework"],
            toolchain_version=data.get("toolchain_version"),
            framework_version=data.get("framework_version"),
            platform_version=data.get("platform_version"),
            build_flags=data.get("build_flags", []),
            lib_deps=data.get("lib_deps", []),
            source_files_hash=data.get("source_files_hash"),
            fbuild_version=data.get("fbuild_version"),
        )

    def save(self, path: Path) -> None:
        """Save build state to JSON file.

        Args:
            path: Path to build_state.json file
        """
        import logging
        path.parent.mkdir(parents=True, exist_ok=True)
        logging.debug(f"[BUILD_STATE] Saving build state to {path}")
        try:
            with open(path, "w", encoding="utf-8") as f:
                json.dump(self.to_dict(), f, indent=2)
            logging.debug(f"[BUILD_STATE] Build state saved successfully (fbuild_version={self.fbuild_version})")
        except KeyboardInterrupt:
            import _thread
            _thread.interrupt_main()
            raise
        except Exception as e:
            logging.error(f"[BUILD_STATE] Failed to save build state: {e}")
            raise

    @classmethod
    def load(cls, path: Path) -> Optional["BuildState"]:
        """Load build state from JSON file.

        Args:
            path: Path to build_state.json file

        Returns:
            BuildState instance or None if file doesn't exist
        """
        import logging
        if not path.exists():
            logging.debug(f"[BUILD_STATE] State file does not exist: {path}")
            return None

        try:
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
            state = cls.from_dict(data)
            logging.debug(f"[BUILD_STATE] Loaded state from {path} (fbuild_version={state.fbuild_version})")
            return state
        except (json.JSONDecodeError, KeyError) as e:
            # Corrupted state file - return None to trigger rebuild
            logging.warning(f"[BUILD_STATE] Corrupted state file {path}: {e}")
            return None

    def compare(self, other: Optional["BuildState"]) -> Tuple[bool, List[str]]:
        """Compare this state with another state.

        Args:
            other: Previous build state (or None if no previous build)

        Returns:
            Tuple of (needs_rebuild, reasons)
            - needs_rebuild: True if build needs to be invalidated
            - reasons: List of human-readable reasons for invalidation
        """
        if other is None:
            return True, ["No previous build state found"]

        reasons = []

        # Check fbuild version changes (first - most important for cache invalidation)
        if self.fbuild_version != other.fbuild_version:
            reasons.append(
                f"fbuild version changed: {other.fbuild_version} -> {self.fbuild_version}"
            )

        # Check platformio.ini changes
        if self.platformio_ini_hash != other.platformio_ini_hash:
            reasons.append("platformio.ini has changed")

        # Check platform changes
        if self.platform != other.platform:
            reasons.append(f"Platform changed: {other.platform} -> {self.platform}")

        # Check board changes
        if self.board != other.board:
            reasons.append(f"Board changed: {other.board} -> {self.board}")

        # Check framework changes
        if self.framework != other.framework:
            reasons.append(f"Framework changed: {other.framework} -> {self.framework}")

        # Check toolchain version changes
        if self.toolchain_version != other.toolchain_version:
            reasons.append(
                f"Toolchain version changed: {other.toolchain_version} -> {self.toolchain_version}"
            )

        # Check framework version changes
        if self.framework_version != other.framework_version:
            reasons.append(
                f"Framework version changed: {other.framework_version} -> {self.framework_version}"
            )

        # Check platform version changes
        if self.platform_version != other.platform_version:
            reasons.append(
                f"Platform version changed: {other.platform_version} -> {self.platform_version}"
            )

        # Check build flags changes
        if set(self.build_flags) != set(other.build_flags):
            reasons.append("Build flags have changed")

        # Check library dependencies changes
        if set(self.lib_deps) != set(other.lib_deps):
            reasons.append("Library dependencies have changed")

        # Check source files changes
        if self.source_files_hash != other.source_files_hash:
            reasons.append("Source files have changed")

        needs_rebuild = len(reasons) > 0
        return needs_rebuild, reasons


class BuildStateTracker:
    """Tracks build state for cache invalidation."""

    def __init__(self, build_dir: Path):
        """Initialize build state tracker.

        Args:
            build_dir: Build directory (.fbuild/build/{env_name})
        """
        self.build_dir = Path(build_dir)
        self.state_file = self.build_dir / "build_state.json"

    @staticmethod
    def hash_file(path: Path) -> str:
        """Calculate SHA256 hash of a file.

        Args:
            path: Path to file

        Returns:
            SHA256 hash as hex string
        """
        sha256 = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                sha256.update(chunk)
        return sha256.hexdigest()

    @staticmethod
    def hash_source_files(source_dir: Path) -> str:
        """Calculate combined SHA256 hash of all source files in a directory.

        Scans for .ino, .cpp, .c, .h, .hpp files and creates a combined hash
        based on file paths (sorted) and their contents. This ensures that
        any change to source files invalidates the build cache.

        Args:
            source_dir: Directory to scan for source files

        Returns:
            SHA256 hash as hex string representing all source files
        """
        sha256 = hashlib.sha256()
        source_extensions = {".ino", ".cpp", ".c", ".h", ".hpp", ".cc", ".cxx"}

        # Find all source files recursively
        source_files = []
        if source_dir.exists():
            for ext in source_extensions:
                source_files.extend(source_dir.rglob(f"*{ext}"))

        # Sort by path for deterministic ordering
        source_files = sorted(source_files)

        # Hash each file's path and content
        for file_path in source_files:
            # Include relative path in hash (so renames are detected)
            try:
                rel_path = file_path.relative_to(source_dir)
            except ValueError:
                rel_path = file_path
            sha256.update(str(rel_path).encode("utf-8"))

            # Hash file content
            try:
                with open(file_path, "rb") as f:
                    for chunk in iter(lambda: f.read(8192), b""):
                        sha256.update(chunk)
            except (OSError, IOError):
                # If we can't read a file, include its path and mtime
                # This ensures the hash changes if the file becomes readable
                try:
                    mtime = file_path.stat().st_mtime
                    sha256.update(f"UNREADABLE:{mtime}".encode("utf-8"))
                except (OSError, IOError):
                    sha256.update(b"MISSING")

        return sha256.hexdigest()

    def create_state(
        self,
        platformio_ini_path: Path,
        platform: str,
        board: str,
        framework: str,
        toolchain_version: Optional[str] = None,
        framework_version: Optional[str] = None,
        platform_version: Optional[str] = None,
        build_flags: Optional[List[str]] = None,
        lib_deps: Optional[List[str]] = None,
        source_dir: Optional[Path] = None,
    ) -> BuildState:
        """Create a BuildState from current configuration.

        Args:
            platformio_ini_path: Path to platformio.ini file
            platform: Platform name
            board: Board ID
            framework: Framework name
            toolchain_version: Toolchain version
            framework_version: Framework version
            platform_version: Platform version
            build_flags: Build flags from platformio.ini
            lib_deps: Library dependencies from platformio.ini
            source_dir: Directory containing source files (.ino, .cpp, .c, .h)

        Returns:
            BuildState instance representing current configuration
        """
        # Hash platformio.ini
        ini_hash = self.hash_file(platformio_ini_path)

        # Hash source files if directory provided
        source_hash = None
        if source_dir is not None:
            source_hash = self.hash_source_files(source_dir)

        return BuildState(
            platformio_ini_hash=ini_hash,
            platform=platform,
            board=board,
            framework=framework,
            toolchain_version=toolchain_version,
            framework_version=framework_version,
            platform_version=platform_version,
            build_flags=build_flags,
            lib_deps=lib_deps,
            source_files_hash=source_hash,
        )

    def load_previous_state(self) -> Optional[BuildState]:
        """Load the previous build state.

        Returns:
            Previous BuildState or None if no previous build
        """
        return BuildState.load(self.state_file)

    def save_state(self, state: BuildState) -> None:
        """Save the current build state.

        Args:
            state: BuildState to save
        """
        state.save(self.state_file)

    def check_invalidation(
        self,
        platformio_ini_path: Path,
        platform: str,
        board: str,
        framework: str,
        toolchain_version: Optional[str] = None,
        framework_version: Optional[str] = None,
        platform_version: Optional[str] = None,
        build_flags: Optional[List[str]] = None,
        lib_deps: Optional[List[str]] = None,
        source_dir: Optional[Path] = None,
    ) -> Tuple[bool, List[str], BuildState]:
        """Check if build cache should be invalidated.

        Args:
            platformio_ini_path: Path to platformio.ini file
            platform: Current platform name
            board: Current board ID
            framework: Current framework name
            toolchain_version: Current toolchain version
            framework_version: Current framework version
            platform_version: Current platform version
            build_flags: Current build flags
            lib_deps: Current library dependencies
            source_dir: Directory containing source files (.ino, .cpp, .c, .h)

        Returns:
            Tuple of (needs_rebuild, reasons, current_state)
            - needs_rebuild: True if build should be invalidated
            - reasons: List of reasons for invalidation
            - current_state: Current BuildState (for saving after build)
        """
        # Create current state
        current_state = self.create_state(
            platformio_ini_path=platformio_ini_path,
            platform=platform,
            board=board,
            framework=framework,
            toolchain_version=toolchain_version,
            framework_version=framework_version,
            platform_version=platform_version,
            build_flags=build_flags,
            lib_deps=lib_deps,
            source_dir=source_dir,
        )

        # Load previous state
        previous_state = self.load_previous_state()

        # Compare states
        needs_rebuild, reasons = current_state.compare(previous_state)

        return needs_rebuild, reasons, current_state
