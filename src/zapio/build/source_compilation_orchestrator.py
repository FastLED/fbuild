"""
Source file compilation orchestration for Zapio build system.

This module handles the orchestration of compiling multiple source files,
including caching, progress reporting, and error handling. It provides a
higher-level interface over the low-level ICompiler interface.
"""

from pathlib import Path
from typing import List

from .compiler import ICompiler, CompilerError


class SourceCompilationOrchestratorError(Exception):
    """Exception raised for source compilation orchestration errors."""
    pass


class SourceCompilationOrchestrator:
    """
    Orchestrates compilation of source files with caching and progress reporting.

    This class provides a higher-level interface for compiling multiple source
    files, handling:
    - Incremental compilation with caching
    - Progress reporting and verbose output
    - Error handling and reporting
    - Categorization by source type (sketch, core, variant)

    Example usage:
        orchestrator = SourceCompilationOrchestrator(verbose=True)
        objects = orchestrator.compile_sources(
            compiler=compiler,
            sources=[Path("main.cpp"), Path("utils.cpp")],
            output_dir=Path(".zapio/build/uno/src"),
            source_type="sketch"
        )
    """

    def __init__(self, verbose: bool = False):
        """
        Initialize source compilation orchestrator.

        Args:
            verbose: Enable verbose output
        """
        self.verbose = verbose

    def compile_sources(
        self,
        compiler: ICompiler,
        sources: List[Path],
        output_dir: Path,
        source_type: str
    ) -> List[Path]:
        """
        Compile list of source files.

        Compiles each source file to an object file, using cached objects when
        possible. Reports progress based on verbose setting.

        Args:
            compiler: Compiler instance
            sources: List of source files to compile
            output_dir: Output directory for object files
            source_type: Type of sources for logging (e.g., 'sketch', 'core', 'variant')

        Returns:
            List of compiled object file paths

        Raises:
            SourceCompilationOrchestratorError: If compilation fails
        """
        # Ensure output directory exists
        output_dir.mkdir(parents=True, exist_ok=True)

        objects = []

        for source in sources:
            # Generate output object filename
            obj_name = source.stem + '.o'
            obj_path = output_dir / obj_name

            # Check if rebuild needed (incremental compilation)
            if not compiler.needs_rebuild(source, obj_path):
                if self.verbose:
                    print(f"      [{source_type}] {source.name} (cached)")
                objects.append(obj_path)
                continue

            # Compile source file
            if self.verbose:
                print(f"      [{source_type}] {source.name}")

            try:
                result = compiler.compile(source, obj_path)

                if not result.success:
                    raise SourceCompilationOrchestratorError(
                        f"Compilation failed for {source}:\n{result.stderr}"
                    )

                objects.append(obj_path)

            except CompilerError as e:
                raise SourceCompilationOrchestratorError(
                    f"Compilation failed for {source}: {e}"
                )

        return objects

    def compile_multiple_groups(
        self,
        compiler: ICompiler,
        sketch_sources: List[Path],
        core_sources: List[Path],
        variant_sources: List[Path],
        src_build_dir: Path,
        core_build_dir: Path
    ) -> 'MultiGroupCompilationResult':
        """
        Compile multiple groups of sources (sketch, core, variant).

        Convenience method for compiling all source groups in a typical build.

        Args:
            compiler: Compiler instance
            sketch_sources: List of sketch source files
            core_sources: List of Arduino core source files
            variant_sources: List of variant source files
            src_build_dir: Build directory for sketch sources
            core_build_dir: Build directory for core and variant sources

        Returns:
            MultiGroupCompilationResult with all compiled objects

        Raises:
            SourceCompilationOrchestratorError: If any compilation fails
        """
        # Compile sketch sources
        sketch_objects = self.compile_sources(
            compiler,
            sketch_sources,
            src_build_dir,
            "sketch"
        )

        # Compile core sources
        core_objects = self.compile_sources(
            compiler,
            core_sources,
            core_build_dir,
            "core"
        )

        # Compile variant sources
        variant_objects = self.compile_sources(
            compiler,
            variant_sources,
            core_build_dir,
            "variant"
        )

        # Combine core and variant objects
        all_core_objects = core_objects + variant_objects

        if self.verbose:
            total_objects = len(sketch_objects) + len(all_core_objects)
            print(f"      Compiled {total_objects} objects")

        return MultiGroupCompilationResult(
            sketch_objects=sketch_objects,
            core_objects=core_objects,
            variant_objects=variant_objects,
            all_core_objects=all_core_objects
        )


class MultiGroupCompilationResult:
    """
    Result of compiling multiple source groups.

    Contains object files organized by source type.
    """

    def __init__(
        self,
        sketch_objects: List[Path],
        core_objects: List[Path],
        variant_objects: List[Path],
        all_core_objects: List[Path]
    ):
        """
        Initialize multi-group compilation result.

        Args:
            sketch_objects: Compiled sketch object files
            core_objects: Compiled core object files
            variant_objects: Compiled variant object files
            all_core_objects: Combined core and variant object files
        """
        self.sketch_objects = sketch_objects
        self.core_objects = core_objects
        self.variant_objects = variant_objects
        self.all_core_objects = all_core_objects
