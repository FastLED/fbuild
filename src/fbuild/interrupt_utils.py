"""Utilities for handling KeyboardInterrupt in try-except blocks.

This module provides utilities to ensure KeyboardInterrupt is properly
propagated to the main thread when caught in exception handlers.
"""

import _thread
from typing import NoReturn


def handle_keyboard_interrupt_properly(ke: KeyboardInterrupt) -> NoReturn:
    """Handle KeyboardInterrupt by propagating it to the main thread.

    This utility ensures that KeyboardInterrupt is properly handled in try-except
    blocks by calling _thread.interrupt_main() before re-raising the exception.

    Usage:
        try:
            # Some code that might be interrupted
            pass
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception:
            # Handle other exceptions
            pass

    Args:
        ke: The KeyboardInterrupt exception to handle

    Raises:
        KeyboardInterrupt: Always re-raises the exception after handling
    """
    _thread.interrupt_main()
    raise ke
