"""
Build system components for Zapio.

This module provides the build system implementation including:
- Source file discovery and preprocessing
- Compilation (avr-gcc/avr-g++)
- Linking (avr-gcc linker, avr-objcopy)
- Build orchestration
"""

from .source_scanner import SourceScanner, SourceCollection

__all__ = [
    'SourceScanner',
    'SourceCollection',
]

# Import base classes
try:
    from .orchestrator import (  # noqa: F401
        IBuildOrchestrator,
        BuildResult,
        BuildOrchestratorError
    )
    __all__.extend(['IBuildOrchestrator', 'BuildResult', 'BuildOrchestratorError'])
except ImportError:
    pass

try:
    from .compiler import ICompiler, CompilerError, ILinker, LinkerError  # noqa: F401
    __all__.extend(['ICompiler', 'CompilerError', 'ILinker', 'LinkerError'])
except ImportError:
    pass

# Import platform-specific implementations
try:
    from .avr_compiler import CompilerAVR  # noqa: F401
    __all__.append('CompilerAVR')
except ImportError:
    pass

try:
    from .linker import LinkerAVR  # noqa: F401
    __all__.append('LinkerAVR')
except ImportError:
    pass

try:
    from .avr_orchestrator import BuildOrchestratorAVR  # noqa: F401
    __all__.append('BuildOrchestratorAVR')
except ImportError:
    pass

try:
    from .esp32_orchestrator import OrchestratorESP32  # noqa: F401
    __all__.append('OrchestratorESP32')
except ImportError:
    pass

try:
    from .binary_generator import BinaryGenerator  # noqa: F401
    __all__.append('BinaryGenerator')
except ImportError:
    pass

try:
    from .build_utils import SizeInfoPrinter  # noqa: F401
    __all__.append('SizeInfoPrinter')
except ImportError:
    pass

try:
    from .flag_builder import FlagBuilder  # noqa: F401
    __all__.append('FlagBuilder')
except ImportError:
    pass

try:
    from .compilation_executor import CompilationExecutor  # noqa: F401
    __all__.append('CompilationExecutor')
except ImportError:
    pass

try:
    from .archive_creator import ArchiveCreator  # noqa: F401
    __all__.append('ArchiveCreator')
except ImportError:
    pass

try:
    from .library_dependency_processor import (  # noqa: F401
        LibraryDependencyProcessor,
        LibraryProcessingResult
    )
    __all__.extend(['LibraryDependencyProcessor', 'LibraryProcessingResult'])
except ImportError:
    pass

try:
    from .source_compilation_orchestrator import (  # noqa: F401
        SourceCompilationOrchestrator,
        SourceCompilationOrchestratorError,
        MultiGroupCompilationResult
    )
    __all__.extend([
        'SourceCompilationOrchestrator',
        'SourceCompilationOrchestratorError',
        'MultiGroupCompilationResult'
    ])
except ImportError:
    pass

try:
    from .build_component_factory import BuildComponentFactory  # noqa: F401
    __all__.append('BuildComponentFactory')
except ImportError:
    pass
