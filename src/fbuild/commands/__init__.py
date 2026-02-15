"""Command implementations for fbuild CLI.

This package contains implementations of fbuild commands that are too
complex to fit in the main cli.py file.
"""

from fbuild.commands.purge import purge_packages

__all__ = ["purge_packages"]
