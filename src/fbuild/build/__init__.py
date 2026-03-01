"""
Build system components for Fbuild.

This module provides the build system implementation including:
- Source file discovery and preprocessing
- Compilation (avr-gcc/avr-g++)
- Linking (avr-gcc linker, avr-objcopy)
- Build orchestration
"""

from fbuild.build.source_scanner import SourceCollection, SourceScanner

__all__ = [
    "SourceScanner",
    "SourceCollection",
]

# Import base classes
try:
    from fbuild.build.orchestrator import BuildOrchestratorError, BuildResult, IBuildOrchestrator

    __all__.extend(["IBuildOrchestrator", "BuildResult", "BuildOrchestratorError"])
except ImportError:
    pass

try:
    from fbuild.build.compiler import CompilerError, ICompiler, ILinker, LinkerError  # noqa: F401

    __all__.extend(["ICompiler", "CompilerError", "ILinker", "LinkerError"])
except ImportError:
    pass

# Import platform-specific implementations
try:
    from fbuild.build.compiler_avr import CompilerAVR  # noqa: F401

    __all__.append("CompilerAVR")
except ImportError:
    pass

try:
    from fbuild.build.linker import LinkerAVR  # noqa: F401

    __all__.append("LinkerAVR")
except ImportError:
    pass

try:
    from fbuild.build.orchestrator_avr import BuildOrchestratorAVR  # noqa: F401

    __all__.append("BuildOrchestratorAVR")
except ImportError:
    pass

try:
    from fbuild.build.orchestrator_esp32 import OrchestratorESP32  # noqa: F401

    __all__.append("OrchestratorESP32")
except ImportError:
    pass

try:
    from fbuild.build.binary_generator import BinaryGenerator  # noqa: F401

    __all__.append("BinaryGenerator")
except ImportError:
    pass

try:
    from fbuild.build.build_utils import SizeInfoPrinter  # noqa: F401

    __all__.append("SizeInfoPrinter")
except ImportError:
    pass

try:
    from fbuild.build.flag_builder import FlagBuilder  # noqa: F401

    __all__.append("FlagBuilder")
except ImportError:
    pass

try:
    from fbuild.build.compilation_executor import CompilationExecutor  # noqa: F401

    __all__.append("CompilationExecutor")
except ImportError:
    pass

try:
    from fbuild.build.archive_creator import ArchiveCreator  # noqa: F401

    __all__.append("ArchiveCreator")
except ImportError:
    pass

try:
    from fbuild.build.library_dependency_processor import LibraryDependencyProcessor, LibraryProcessingResult

    __all__.extend(["LibraryDependencyProcessor", "LibraryProcessingResult"])
except ImportError:
    pass

try:
    from fbuild.build.source_compilation_orchestrator import MultiGroupCompilationResult, SourceCompilationOrchestrator, SourceCompilationOrchestratorError

    __all__.extend(["SourceCompilationOrchestrator", "SourceCompilationOrchestratorError", "MultiGroupCompilationResult"])
except ImportError:
    pass

try:
    from fbuild.build.build_component_factory import BuildComponentFactory  # noqa: F401

    __all__.append("BuildComponentFactory")
except ImportError:
    pass
