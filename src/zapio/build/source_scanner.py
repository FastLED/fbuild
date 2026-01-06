"""
Source file discovery and Arduino .ino preprocessing.

This module handles:
- Scanning project directories for source files (.ino, .cpp, .c, .h)
- Converting .ino files to .cpp with Arduino preprocessing
- Discovering Arduino core source files
- Discovering board variant source files
- Building source file collections for compilation
"""

import re
from pathlib import Path
from typing import List, Optional
from dataclasses import dataclass


@dataclass
class SourceCollection:
    """Collection of source files categorized by type."""

    sketch_sources: List[Path]      # User sketch .cpp/.c files
    core_sources: List[Path]        # Arduino core .cpp/.c files
    variant_sources: List[Path]     # Board variant .cpp/.c files
    headers: List[Path]              # All .h files for dependency tracking

    def all_sources(self) -> List[Path]:
        """Get all source files combined."""
        return self.sketch_sources + self.core_sources + self.variant_sources


class SourceScanner:
    """
    Scans for source files and preprocesses Arduino .ino files.

    The scanner:
    1. Finds all .ino/.cpp/.c files in the project
    2. Converts .ino files to .cpp with Arduino preprocessing
    3. Scans Arduino core directory for core library sources
    4. Scans variant directory for board-specific sources
    5. Returns organized SourceCollection for compilation
    """

    def __init__(self, project_dir: Path, build_dir: Path):
        """
        Initialize source scanner.

        Args:
            project_dir: Root project directory
            build_dir: Build output directory (for generated .cpp files)
        """
        self.project_dir = Path(project_dir)
        self.build_dir = Path(build_dir)

    def scan(
        self,
        src_dir: Optional[Path] = None,
        core_dir: Optional[Path] = None,
        variant_dir: Optional[Path] = None
    ) -> SourceCollection:
        """
        Scan for all source files.

        Args:
            src_dir: Project source directory (defaults to project_dir/src)
            core_dir: Arduino core directory (e.g., cores/arduino)
            variant_dir: Board variant directory (e.g., variants/standard)

        Returns:
            SourceCollection with all discovered sources
        """
        # Default source directory
        if src_dir is None:
            src_dir = self.project_dir / 'src'
        else:
            src_dir = Path(src_dir)

        # Scan sketch sources
        sketch_sources = self._scan_sketch_sources(src_dir)

        # Scan core sources if provided
        core_sources = []
        if core_dir:
            core_sources = self._scan_core_sources(Path(core_dir))

        # Scan variant sources if provided
        variant_sources = []
        if variant_dir:
            variant_sources = self._scan_variant_sources(Path(variant_dir))

        # Find all headers
        headers = self._find_headers(src_dir)

        return SourceCollection(
            sketch_sources=sketch_sources,
            core_sources=core_sources,
            variant_sources=variant_sources,
            headers=headers
        )

    def _scan_sketch_sources(self, src_dir: Path) -> List[Path]:
        """
        Scan sketch directory for source files.

        Finds .ino, .cpp, and .c files. Preprocesses .ino files to .cpp.

        Args:
            src_dir: Source directory to scan

        Returns:
            List of source file paths (with .ino converted to .cpp)
        """
        if not src_dir.exists():
            return []

        sources = []

        # Find all .ino files and preprocess them
        ino_files = sorted(src_dir.glob('*.ino'))
        if ino_files:
            # Preprocess .ino files (may be multiple, concatenate them)
            cpp_file = self._preprocess_ino_files(ino_files)
            sources.append(cpp_file)

        # Find existing .cpp and .c files
        for pattern in ['*.cpp', '*.c']:
            sources.extend(sorted(src_dir.glob(pattern)))

        # Recursively find sources in subdirectories
        for subdir in src_dir.iterdir():
            if subdir.is_dir():
                for pattern in ['**/*.cpp', '**/*.c']:
                    sources.extend(sorted(subdir.glob(pattern)))

        return sources

    def _scan_core_sources(self, core_dir: Path) -> List[Path]:
        """
        Scan Arduino core directory for source files.

        Args:
            core_dir: Arduino core directory (e.g., cores/arduino)

        Returns:
            List of core source file paths
        """
        if not core_dir.exists():
            return []

        sources = []
        for pattern in ['*.cpp', '*.c']:
            sources.extend(sorted(core_dir.glob(pattern)))

        return sources

    def _scan_variant_sources(self, variant_dir: Path) -> List[Path]:
        """
        Scan board variant directory for source files.

        Args:
            variant_dir: Variant directory (e.g., variants/standard)

        Returns:
            List of variant source file paths
        """
        if not variant_dir.exists():
            return []

        sources = []
        for pattern in ['*.cpp', '*.c']:
            sources.extend(sorted(variant_dir.glob(pattern)))

        return sources

    def _find_headers(self, src_dir: Path) -> List[Path]:
        """
        Find all header files in source directory.

        Args:
            src_dir: Source directory to scan

        Returns:
            List of header file paths
        """
        if not src_dir.exists():
            return []

        headers: set[Path] = set()
        for pattern in ['*.h', '*.hpp']:
            headers.update(src_dir.glob(pattern))
        for pattern in ['**/*.h', '**/*.hpp']:
            headers.update(src_dir.glob(pattern))

        return sorted(list(headers))

    def _preprocess_ino_files(self, ino_files: List[Path]) -> Path:
        """
        Preprocess .ino files to .cpp with Arduino conventions.

        Arduino preprocessing rules:
        1. Concatenate multiple .ino files (alphabetically)
        2. Add #include <Arduino.h> at the top
        3. Extract function prototypes
        4. Add function prototypes before first function definition

        Args:
            ino_files: List of .ino files to preprocess

        Returns:
            Path to generated .cpp file
        """
        # Create output .cpp file
        output_file = self.build_dir / 'sketch.cpp'
        output_file.parent.mkdir(parents=True, exist_ok=True)

        # Concatenate all .ino files
        combined_content = []
        for ino_file in sorted(ino_files):
            combined_content.append(ino_file.read_text(encoding='utf-8'))

        content = '\n\n'.join(combined_content)

        # Extract function prototypes
        prototypes = self._extract_function_prototypes(content)

        # Build final .cpp file
        cpp_lines = [
            '#include <Arduino.h>',
            '',
            '// Function prototypes',
        ]

        # Add prototypes
        for prototype in prototypes:
            cpp_lines.append(prototype)

        cpp_lines.extend([
            '',
            '// Original sketch code',
            content
        ])

        cpp_content = '\n'.join(cpp_lines)
        output_file.write_text(cpp_content, encoding='utf-8')

        return output_file

    def _extract_function_prototypes(self, content: str) -> List[str]:
        """
        Extract function prototypes from Arduino sketch.

        Finds function definitions and generates forward declarations.

        Args:
            content: Sketch source code

        Returns:
            List of function prototype strings
        """
        prototypes = []

        # Pattern to match function definitions
        # Matches: return_type function_name(params) {
        # Ignores: preprocessor directives, class methods, etc.
        pattern = r'^([a-zA-Z_][\w\s\*&:<>,]*?)\s+([a-zA-Z_]\w*)\s*\((.*?)\)\s*\{'

        lines = content.split('\n')
        in_multiline_comment = False

        for line in lines:
            # Track multiline comments
            if '/*' in line:
                in_multiline_comment = True
            if '*/' in line:
                in_multiline_comment = False
                continue

            # Skip comments and preprocessor directives
            if in_multiline_comment:
                continue
            if line.strip().startswith('//'):
                continue
            if line.strip().startswith('#'):
                continue

            # Try to match function definition
            match = re.match(pattern, line.strip())
            if match:
                return_type = match.group(1).strip()
                func_name = match.group(2).strip()
                params = match.group(3).strip()

                # Skip common false positives
                if func_name in ['if', 'while', 'for', 'switch']:
                    continue

                # Generate prototype
                prototype = f"{return_type} {func_name}({params});"
                prototypes.append(prototype)

        return prototypes


class SourceScannerError(Exception):
    """Raised when source scanning fails."""
    pass
