"""Type surface of the compiled PyO3 extension (crates/fbuild-python).

The extension is built by ci/build_backend.py and is not importable from a
plain checkout, so type checkers resolve it through this stub. Only
``__version__`` is typed; every class and function the Rust module exports
is ``Any`` until a generated stub replaces this one, so no signature here can
drift from the Rust source.
"""

from typing import Any

__version__: str

def __getattr__(name: str) -> Any: ...
