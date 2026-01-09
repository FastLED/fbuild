"""Header Trampoline Cache System.

This module implements ordered header trampoline directories that resolve Windows
CreateProcess() command-line length failures caused by excessive GCC -I arguments.

The problem:
- GCC invocations contain hundreds of long -I paths
- sccache expands response files into a single CreateProcess() call
- Windows enforces a hard 32,767 character string-length limit
- This causes build failures with ESP32-C6 (ESP-IDF) projects

The solution:
- Create "trampoline" header files that redirect to original headers
- Use ordered, short directory names (e.g., C:/inc/001, C:/inc/002, ...)
- Preserve include ordering semantics (critical for ESP-IDF)
- Maintain full sccache compatibility with deterministic generation

Design:
    Original:  -I D:/toolchains/esp-idf/components/freertos/include
               -I D:/toolchains/esp-idf/components/driver/include
               -I D:/build/project/config

    Rewritten: -I C:/inc/001
               -I C:/inc/002
               -I C:/inc/003

    Where C:/inc/001/freertos/FreeRTOS.h contains:
        #pragma once
        #include "D:/toolchains/esp-idf/components/freertos/include/freertos/FreeRTOS.h"

Properties:
- Include order is identical (preserves ESP-IDF correctness)
- Header collision behavior unchanged
- Generated headers resolve correctly
- Only string length of -I arguments changes
- Deterministic and reproducible
- Fully compatible with GCC and sccache
"""

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
    - Generating ordered trampoline directories
    - Creating trampoline header files
    - Managing cache invalidation
    - Providing rewritten include paths
    """

    def __init__(self, cache_root: Optional[Path] = None, show_progress: bool = True):
        """Initialize header trampoline cache.

        Args:
            cache_root: Root directory for trampoline cache (default: C:/inc on Windows)
            show_progress: Whether to show cache generation progress
        """
        self.show_progress = show_progress

        # Determine cache root
        if cache_root is None:
            if platform.system() == "Windows":
                # Use short, root-level path on Windows
                self.cache_root = Path("C:/inc")
            else:
                # Use /tmp/inc on Linux/Mac
                self.cache_root = Path("/tmp/inc")
        else:
            self.cache_root = Path(cache_root)

        # Metadata file tracks cache state
        self.metadata_file = self.cache_root / ".metadata.json"

    def needs_regeneration(self, include_paths: List[Path]) -> bool:
        """Check if trampoline cache needs regeneration.

        Cache needs regeneration when:
        - Cache doesn't exist
        - Include path list changed
        - Include path order changed
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
            raise
        except Exception:
            return True

        # Check if include paths changed
        current_hash = self._compute_include_hash(include_paths)
        cached_hash = metadata.get("include_hash", "")

        return current_hash != cached_hash

    def generate_trampolines(
        self, include_paths: List[Path], exclude_patterns: Optional[List[str]] = None
    ) -> List[Path]:
        """Generate trampoline cache and return rewritten include paths.

        This is the main entry point for the trampoline system.

        Args:
            include_paths: Ordered list of original include directory paths
            exclude_patterns: Optional list of path patterns to exclude from trampolining.
                            Paths matching these patterns will be returned as-is.

        Returns:
            Ordered list of trampoline directory paths (short), with excluded paths
            preserved as original paths in their original positions

        Raises:
            TrampolineCacheError: If trampoline generation fails
        """
        # Filter out excluded paths
        filtered_paths = []
        excluded_indices = set()

        if exclude_patterns:
            for idx, path in enumerate(include_paths):
                path_str = str(path)
                excluded = False

                for pattern in exclude_patterns:
                    if pattern in path_str:
                        excluded = True
                        excluded_indices.add(idx)
                        break

                if not excluded:
                    filtered_paths.append(path)
        else:
            filtered_paths = include_paths

        # Check if regeneration needed (use filtered paths for cache validation)
        if not self.needs_regeneration(filtered_paths):
            if self.show_progress:
                excluded_count = len(include_paths) - len(filtered_paths)
                if excluded_count > 0:
                    print(
                        f"[trampolines] Using existing cache at {self.cache_root} "
                        + f"(excluding {excluded_count} paths)"
                    )
                else:
                    print(f"[trampolines] Using existing cache at {self.cache_root}")
            return self._load_and_merge_trampoline_paths(
                include_paths, filtered_paths, excluded_indices
            )

        if self.show_progress:
            excluded_count = len(include_paths) - len(filtered_paths)
            if excluded_count > 0:
                print(
                    f"[trampolines] Generating cache for {len(filtered_paths)} include paths "
                    + f"(excluding {excluded_count} paths)..."
                )
            else:
                print(
                    f"[trampolines] Generating cache for {len(include_paths)} include paths..."
                )

        try:
            # Clear existing cache
            self._clear_cache()

            # Create cache root
            self.cache_root.mkdir(parents=True, exist_ok=True)

            # Generate trampoline directories (one per non-excluded include path)
            trampoline_paths = []
            for idx, original_path in enumerate(filtered_paths):
                # Create short numbered directory (001, 002, ...)
                layer_name = f"{idx:03d}"
                trampoline_dir = self.cache_root / layer_name
                trampoline_dir.mkdir(parents=True, exist_ok=True)

                # Generate trampolines for all headers under original_path
                self._generate_layer_trampolines(original_path, trampoline_dir)

                trampoline_paths.append(trampoline_dir)

            # Save metadata
            self._save_metadata(filtered_paths, trampoline_paths)

            if self.show_progress:
                print(f"[trampolines] Generated cache at {self.cache_root}")

            # Merge trampolines with excluded paths in original positions
            return self._merge_paths(
                include_paths, filtered_paths, trampoline_paths, excluded_indices
            )

        except KeyboardInterrupt:
            raise
        except Exception as e:
            raise TrampolineCacheError(
                f"Failed to generate trampoline cache: {e}"
            ) from e

    def _generate_layer_trampolines(
        self, original_path: Path, trampoline_dir: Path
    ) -> None:
        """Generate trampoline headers for a single include layer.

        Args:
            original_path: Original include directory
            trampoline_dir: Trampoline directory for this layer

        Raises:
            TrampolineCacheError: If trampoline generation fails
        """
        if not original_path.exists():
            # Skip non-existent paths (may be generated later)
            return

        # Find all header files under original_path
        header_extensions = {".h", ".hpp", ".hxx", ".h++", ".hh"}
        header_files = []

        try:
            for ext in header_extensions:
                header_files.extend(original_path.rglob(f"*{ext}"))
        except KeyboardInterrupt:
            raise
        except Exception as e:
            if self.show_progress:
                print(f"[trampolines] Warning: Failed to scan {original_path}: {e}")
            return

        # Generate trampoline for each header
        for header_file in header_files:
            try:
                # Calculate relative path from original_path
                rel_path = header_file.relative_to(original_path)

                # Create trampoline path
                trampoline_file = trampoline_dir / rel_path
                trampoline_file.parent.mkdir(parents=True, exist_ok=True)

                # Generate trampoline content
                # Use forward slashes for portability (GCC accepts both on Windows)
                original_abs = header_file.resolve()
                original_str = str(original_abs).replace("\\", "/")

                trampoline_content = f'#pragma once\n#include "{original_str}"\n'

                # Write trampoline file
                with open(trampoline_file, "w", encoding="utf-8") as f:
                    f.write(trampoline_content)

            except KeyboardInterrupt:
                raise
            except Exception as e:
                if self.show_progress:
                    print(
                        f"[trampolines] Warning: Failed to create trampoline for {header_file}: {e}"
                    )
                continue

    def _compute_include_hash(self, include_paths: List[Path]) -> str:
        """Compute hash of include path list for cache validation.

        Args:
            include_paths: Ordered list of include paths

        Returns:
            SHA256 hash of the include path list
        """
        # Convert paths to strings and join with newlines
        path_str = "\n".join(str(p.resolve()) for p in include_paths)
        return hashlib.sha256(path_str.encode("utf-8")).hexdigest()

    def _save_metadata(
        self, include_paths: List[Path], trampoline_paths: List[Path]
    ) -> None:
        """Save cache metadata.

        Args:
            include_paths: Original include paths
            trampoline_paths: Generated trampoline paths
        """
        metadata = {
            "version": "1.0",
            "include_hash": self._compute_include_hash(include_paths),
            "original_paths": [str(p.resolve()) for p in include_paths],
            "trampoline_paths": [str(p) for p in trampoline_paths],
            "platform": platform.system(),
        }

        with open(self.metadata_file, "w") as f:
            json.dump(metadata, f, indent=2)

    def _load_trampoline_paths(self, include_paths: List[Path]) -> List[Path]:
        """Load trampoline paths from metadata.

        Args:
            include_paths: Original include paths (for validation)

        Returns:
            List of trampoline directory paths
        """
        with open(self.metadata_file, "r") as f:
            metadata = json.load(f)

        return [Path(p) for p in metadata["trampoline_paths"]]

    def _load_and_merge_trampoline_paths(
        self,
        include_paths: List[Path],
        filtered_paths: List[Path],
        excluded_indices: set,
    ) -> List[Path]:
        """Load trampoline paths and merge with excluded paths.

        Args:
            include_paths: Original include paths (all)
            filtered_paths: Filtered include paths (non-excluded)
            excluded_indices: Set of indices that were excluded

        Returns:
            List of paths with trampolines and original excluded paths
        """
        trampoline_paths = self._load_trampoline_paths(filtered_paths)
        return self._merge_paths(
            include_paths, filtered_paths, trampoline_paths, excluded_indices
        )

    def _merge_paths(
        self,
        include_paths: List[Path],
        filtered_paths: List[Path],
        trampoline_paths: List[Path],
        excluded_indices: set,
    ) -> List[Path]:
        """Merge trampoline paths with excluded paths in original positions.

        Args:
            include_paths: Original include paths (all)
            filtered_paths: Filtered include paths (non-excluded)
            trampoline_paths: Generated trampoline paths
            excluded_indices: Set of indices that were excluded

        Returns:
            List of paths with trampolines for non-excluded and originals for excluded
        """
        result = []
        filtered_idx = 0

        for idx, path in enumerate(include_paths):
            if idx in excluded_indices:
                # Use original path for excluded
                result.append(path)
            else:
                # Use trampoline path
                result.append(trampoline_paths[filtered_idx])
                filtered_idx += 1

        return result

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
                raise
            except Exception:
                pass

        return info
