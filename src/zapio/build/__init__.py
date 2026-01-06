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

# Import other components if they exist
try:
    from .compiler import Compiler  # noqa: F401
    __all__.append('Compiler')
except ImportError:
    pass

try:
    from .linker import Linker  # noqa: F401
    __all__.append('Linker')
except ImportError:
    pass

try:
    from .orchestrator import BuildOrchestrator  # noqa: F401
    __all__.append('BuildOrchestrator')
except ImportError:
    pass
