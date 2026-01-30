"""Header Trampoline Cache System.

This module implements a UNIFIED header trampoline directory that resolves Windows
CreateProcess() command-line length failures caused by excessive GCC -I arguments.

The problem:
- GCC invocations contain hundreds of long -I paths
- sccache expands response files into a single CreateProcess() call
- Windows enforces a hard 32,767 character string-length limit
- This causes build failures with ESP32-C6 (ESP-IDF) projects

The solution (v3.0 - Unified Single Directory):
- Create a SINGLE "trampoline" directory containing ALL headers
- Use ONE -I directive instead of hundreds
- Preserve include ordering semantics using ordered subdirectories (000/, 001/, ...)
- The GCC -I ordering is achieved by having ONE root with ordered subdirs

Design (v3.0):
    Original:  -I D:/toolchains/esp-idf/components/freertos/include
               -I D:/toolchains/esp-idf/components/driver/include
               -I D:/build/project/config
               (305+ -I directives, ~25,000 chars)

    Rewritten: -I ~/.fbuild/trampolines/esp32c6
               (1 -I directive, ~50 chars)

    Where ~/.fbuild/trampolines/esp32c6/freertos/FreeRTOS.h contains:
        #pragma once
        #include "D:/toolchains/esp-idf/components/freertos/include/freertos/FreeRTOS.h"

    The design flattens all headers into the root, handling conflicts by keeping first occurrence
    (matching GCC's -I ordering semantics where first match wins).

    ~/.fbuild/trampolines/esp32c6/freertos/FreeRTOS.h → first -I path that contains it

    This works because:
    1. Headers in earlier -I paths take precedence (GCC behavior)
    2. We only write the trampoline if it doesn't already exist
    3. Result: first occurrence wins, matching GCC semantics

Properties:
- Include order is preserved (first occurrence wins)
- Only ONE -I argument needed
- Command line length reduced from ~25,000 chars to ~50 chars
- Deterministic and reproducible
- Fully compatible with GCC and sccache
- All fbuild data is centralized in ~/.fbuild
"""

import _thread
import hashlib
import json
import platform
from pathlib import Path
from typing import Any, Dict, List, Optional


class TrampolineCacheError(Exception):
    """Raised when trampoline cache operations fail."""

    pass


class HeaderTrampolineCache:
    """Manages header trampoline cache for reducing command-line length.

    This class handles:
    - Generating a unified trampoline directory
    - Creating trampoline header files
    - Managing cache invalidation
    - Providing a SINGLE rewritten include path
    """

    def __init__(
        self,
        cache_root: Optional[Path] = None,
        show_progress: bool = True,
        mcu_variant: Optional[str] = None,
        framework_version: Optional[str] = None,
        platform_name: str = "esp32",
    ):
        """Initialize header trampoline cache.

        Args:
            cache_root: Root directory for trampoline cache (on Windows, uses ~/.fbuild/trampolines/)
            show_progress: Whether to show cache generation progress
            mcu_variant: MCU variant identifier (e.g., 'esp32c6', 'esp32c3')
            framework_version: Framework version string for cache invalidation
            platform_name: Platform identifier (e.g., 'esp32', 'avr')
        """
        self.show_progress = show_progress
        self.mcu_variant = mcu_variant
        self.framework_version = framework_version
        self.platform_name = platform_name

        # On Windows, use ~/.fbuild/trampolines to keep all fbuild data in one place
        # The UNIFIED design uses a SINGLE directory, resulting in ONE -I directive
        #
        # Path calculation (v3.0 UNIFIED):
        #   With ~/.fbuild/trampolines/{mcu}: ~50 chars TOTAL (1 -I directive)
        #   With old design: ~6,375 chars (375 × 17 chars per -I directive)
        #   Savings: >99% reduction in command-line length
        if platform.system() == "Windows":
            # Use ~/.fbuild/trampolines on Windows for centralized storage
            global_root = Path.home() / ".fbuild" / "trampolines"
            if mcu_variant:
                self.cache_root = global_root / mcu_variant
            else:
                self.cache_root = global_root / "generic"
        elif cache_root is None:
            # Non-Windows without explicit cache_root
            self.cache_root = Path("/tmp/inc")
            if mcu_variant:
                self.cache_root = self.cache_root / mcu_variant
            if show_progress:
                print("[trampolines] Using /tmp/inc for trampoline cache")
        else:
            # Non-Windows with explicit cache_root
            if mcu_variant:
                self.cache_root = cache_root / mcu_variant
            else:
                self.cache_root = cache_root / "generic"

        # Metadata file tracks cache state
        self.metadata_file = self.cache_root / ".metadata.json"

    def needs_regeneration(self, include_paths: List[Path]) -> bool:
        """Check if trampoline cache needs regeneration.

        Cache needs regeneration when:
        - Cache doesn't exist
        - Metadata version changed (forces upgrade)
        - Include path list changed
        - Include path order changed
        - Framework version changed
        - MCU variant changed
        - Platform changed
        - Any original header files changed (not implemented yet)

        Args:
            include_paths: Ordered list of include directory paths

        Returns:
            True if cache needs regeneration
        """
        if not self.cache_root.exists() or not self.metadata_file.exists():
            return True

        # Read existing metadata
        try:
            with open(self.metadata_file, "r") as f:
                metadata = json.load(f)
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception:
            return True

        # Force regeneration on metadata version upgrade (v3.0 for unified design)
        if metadata.get("version", "1.0") != "3.0":
            if self.show_progress:
                print("[trampolines] Metadata version upgrade to v3.0 (unified), regenerating cache")
            return True

        # Check if configuration changed (includes version, MCU, platform in hash)
        current_hash = self._compute_include_hash(include_paths)
        cached_hash = metadata.get("include_hash", "")

        return current_hash != cached_hash

    def generate_trampolines(self, include_paths: List[Path], exclude_patterns: Optional[List[str]] = None) -> List[Path]:
        """Generate trampoline cache and return rewritten include paths.

        This is the main entry point for the trampoline system.

        v3.0 UNIFIED DESIGN: Returns a SINGLE trampoline path that contains ALL headers.

        Args:
            include_paths: Ordered list of original include directory paths
            exclude_patterns: Optional list of path patterns to exclude from trampolining.
                            Paths matching these patterns will be returned as-is.

        Returns:
            List containing:
            - ONE unified trampoline path (for all non-excluded paths)
            - Original paths for excluded patterns (appended at end)

        Raises:
            TrampolineCacheError: If trampoline generation fails
        """
        # Filter out excluded paths
        filtered_paths = []
        excluded_paths = []

        if exclude_patterns:
            for path in include_paths:
                path_str = str(path)
                excluded = False

                for pattern in exclude_patterns:
                    if pattern in path_str:
                        excluded = True
                        excluded_paths.append(path)
                        break

                if not excluded:
                    filtered_paths.append(path)
        else:
            filtered_paths = list(include_paths)

        # Check if regeneration needed (use filtered paths for cache validation)
        if not self.needs_regeneration(filtered_paths):
            if self.show_progress:
                excluded_count = len(excluded_paths)
                if excluded_count > 0:
                    print(f"[trampolines] Using existing UNIFIED cache at {self.cache_root} " + f"(excluding {excluded_count} paths)")
                else:
                    print(f"[trampolines] Using existing UNIFIED cache at {self.cache_root}")
            # Return unified trampoline path + excluded paths
            return [self.cache_root] + excluded_paths

        if self.show_progress:
            excluded_count = len(excluded_paths)
            if excluded_count > 0:
                print(f"[trampolines] Generating UNIFIED cache for {len(filtered_paths)} include paths " + f"(excluding {excluded_count} paths)...")
            else:
                print(f"[trampolines] Generating UNIFIED cache for {len(include_paths)} include paths...")

        try:
            # Clear existing cache
            self._clear_cache()

            # Create cache root
            self.cache_root.mkdir(parents=True, exist_ok=True)

            # Track which headers we've already created trampolines for
            # This ensures "first occurrence wins" semantics matching GCC -I order
            created_trampolines: set = set()
            total_headers = 0
            skipped_duplicates = 0

            # Generate unified trampolines for all headers from all include paths
            for original_path in filtered_paths:
                count, skipped = self._generate_unified_trampolines(original_path, created_trampolines)
                total_headers += count
                skipped_duplicates += skipped

            # Save metadata
            self._save_metadata(filtered_paths, [self.cache_root])

            if self.show_progress:
                print(f"[trampolines] Generated UNIFIED cache at {self.cache_root}")
                print(f"[trampolines] Created {total_headers} trampolines, skipped {skipped_duplicates} duplicates")
                print(f"[trampolines] Command line reduced from {len(filtered_paths)} -I directives to 1")

            # Return unified trampoline path + excluded paths
            return [self.cache_root] + excluded_paths

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            raise TrampolineCacheError(f"Failed to generate trampoline cache: {e}") from e

    def _generate_unified_trampolines(self, original_path: Path, created_trampolines: set) -> tuple:
        """Generate trampoline headers for a single include path into the unified directory.

        Headers are placed directly in self.cache_root, preserving their relative paths.
        If a header already exists (from an earlier include path), it is NOT overwritten,
        matching GCC's -I precedence semantics.

        Args:
            original_path: Original include directory
            created_trampolines: Set of relative paths already created (for deduplication)

        Returns:
            Tuple of (headers_created, headers_skipped)

        Raises:
            TrampolineCacheError: If trampoline generation fails
        """
        if not original_path.exists():
            # Skip non-existent paths (may be generated later)
            return 0, 0

        # Find all header files under original_path
        header_extensions = {".h", ".hpp", ".hxx", ".h++", ".hh"}
        header_files = []

        try:
            for ext in header_extensions:
                header_files.extend(original_path.rglob(f"*{ext}"))
        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            if self.show_progress:
                print(f"[trampolines] Warning: Failed to scan {original_path}: {e}")
            return 0, 0

        headers_created = 0
        headers_skipped = 0

        # Generate trampoline for each header
        for header_file in header_files:
            try:
                # Calculate relative path from original_path
                rel_path = header_file.relative_to(original_path)
                rel_path_str = str(rel_path).replace("\\", "/")

                # Check if we already have a trampoline for this relative path
                # (from an earlier -I directory with higher priority)
                if rel_path_str in created_trampolines:
                    headers_skipped += 1
                    continue

                # Create trampoline path in the unified directory root
                trampoline_file = self.cache_root / rel_path
                trampoline_file.parent.mkdir(parents=True, exist_ok=True)

                # Generate trampoline content
                # Use forward slashes for portability (GCC accepts both on Windows)
                original_abs = header_file.resolve()
                original_str = str(original_abs).replace("\\", "/")

                trampoline_content = f'#pragma once\n#include "{original_str}"\n'

                # Write trampoline file
                with open(trampoline_file, "w", encoding="utf-8") as f:
                    f.write(trampoline_content)

                # Track that we've created this trampoline
                created_trampolines.add(rel_path_str)
                headers_created += 1

            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                if self.show_progress:
                    print(f"[trampolines] Warning: Failed to create trampoline for {header_file}: {e}")
                continue

        return headers_created, headers_skipped

    def _compute_include_hash(self, include_paths: List[Path]) -> str:
        """Compute hash of include path list for cache validation.

        Hash includes paths, framework version, MCU variant, and platform to ensure
        cache invalidation when any of these change.

        Args:
            include_paths: Ordered list of include paths

        Returns:
            SHA256 hash of the include path list and metadata
        """
        components = []

        # Include paths (resolved, normalized)
        path_str = "\n".join(str(p.resolve()) for p in include_paths)
        components.append(path_str)

        # Framework version (cache invalidation on upgrade)
        if self.framework_version:
            components.append(f"framework_version:{self.framework_version}")

        # MCU variant (different MCUs have different headers)
        if self.mcu_variant:
            components.append(f"mcu:{self.mcu_variant}")

        # Platform identifier
        components.append(f"platform:{self.platform_name}")

        combined = "\n".join(components)
        return hashlib.sha256(combined.encode("utf-8")).hexdigest()

    def _save_metadata(self, include_paths: List[Path], trampoline_paths: List[Path]) -> None:
        """Save cache metadata.

        Args:
            include_paths: Original include paths
            trampoline_paths: Generated trampoline paths (now just ONE unified path)
        """
        from datetime import datetime, timezone

        metadata = {
            "version": "3.0",  # BUMPED to 3.0 for unified design
            "include_hash": self._compute_include_hash(include_paths),
            "framework_version": self.framework_version,
            "mcu_variant": self.mcu_variant,
            "platform": self.platform_name,
            "os": platform.system(),
            "original_paths": [str(p.resolve()) for p in include_paths],
            "trampoline_paths": [str(p) for p in trampoline_paths],
            "unified": True,  # NEW: indicates v3.0 unified design
            "created_at": datetime.now(timezone.utc).isoformat(),
        }

        with open(self.metadata_file, "w") as f:
            json.dump(metadata, f, indent=2)

    def _clear_cache(self) -> None:
        """Clear existing trampoline cache."""
        if self.cache_root.exists():
            import shutil

            shutil.rmtree(self.cache_root, ignore_errors=True)

    def get_cache_info(self) -> Dict[str, Any]:
        """Get information about the trampoline cache.

        Returns:
            Dictionary with cache information
        """
        info = {
            "cache_root": str(self.cache_root),
            "exists": self.cache_root.exists(),
            "metadata_exists": self.metadata_file.exists(),
        }

        if self.metadata_file.exists():
            try:
                with open(self.metadata_file, "r") as f:
                    metadata = json.load(f)
                info["metadata"] = metadata
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception:
                pass

        return info
