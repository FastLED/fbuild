"""
Logging utilities for fbuild daemon.

This module provides decorators and utilities to reduce logging verbosity
while maintaining debuggability through automatic function entry/exit logging.
"""

import functools
import logging
from typing import Any, Callable, TypeVar

from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

# Type variable for generic decorator
F = TypeVar("F", bound=Callable[..., Any])


def log_function_calls(logger: logging.Logger | None = None, level: int = logging.DEBUG) -> Callable[[F], F]:
    """Decorator to log function entry and exit.

    This decorator automatically logs when a function is called and when it returns,
    including the function name and arguments. This replaces the need for manual
    logging statements at the beginning and end of functions.

    Args:
        logger: Logger instance to use (defaults to function's module logger)
        level: Logging level to use (default: DEBUG)

    Returns:
        Decorated function

    Example:
        >>> @log_function_calls()
        ... def my_function(arg1: str, arg2: int) -> bool:
        ...     # Function logic here
        ...     return True
    """

    def decorator(func: F) -> F:
        # Get function's module logger if none provided
        func_logger = logger or logging.getLogger(func.__module__)

        @functools.wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            # Format arguments for logging (truncate long strings)
            args_repr = []
            for arg in args:
                arg_str = repr(arg)
                if len(arg_str) > 100:
                    arg_str = arg_str[:97] + "..."
                args_repr.append(arg_str)

            kwargs_repr = []
            for key, value in kwargs.items():
                value_str = repr(value)
                if len(value_str) > 100:
                    value_str = value_str[:97] + "..."
                kwargs_repr.append(f"{key}={value_str}")

            signature = ", ".join(args_repr + kwargs_repr)

            # Log function entry
            func_logger.log(level, f"→ {func.__name__}({signature})")

            try:
                result = func(*args, **kwargs)
                # Log function exit (without result to avoid noise)
                func_logger.log(level, f"← {func.__name__}() completed")
                return result
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                # Log exception exit
                func_logger.log(level, f"← {func.__name__}() raised {type(e).__name__}: {e}")
                raise

        return wrapper  # type: ignore

    return decorator


def log_method_calls(logger: logging.Logger | None = None, level: int = logging.DEBUG) -> Callable[[F], F]:
    """Decorator to log method entry and exit (for class methods).

    Similar to log_function_calls but designed for class methods. Skips logging
    the 'self' parameter to reduce noise.

    Args:
        logger: Logger instance to use (defaults to method's module logger)
        level: Logging level to use (default: DEBUG)

    Returns:
        Decorated method

    Example:
        >>> class MyClass:
        ...     @log_method_calls()
        ...     def my_method(self, arg1: str) -> bool:
        ...         return True
    """

    def decorator(func: F) -> F:
        # Get method's module logger if none provided
        func_logger = logger or logging.getLogger(func.__module__)

        @functools.wraps(func)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            # Skip 'self' parameter (args[0])
            args_repr = []
            for i, arg in enumerate(args):
                if i == 0:  # Skip 'self'
                    continue
                arg_str = repr(arg)
                if len(arg_str) > 100:
                    arg_str = arg_str[:97] + "..."
                args_repr.append(arg_str)

            kwargs_repr = []
            for key, value in kwargs.items():
                value_str = repr(value)
                if len(value_str) > 100:
                    value_str = value_str[:97] + "..."
                kwargs_repr.append(f"{key}={value_str}")

            signature = ", ".join(args_repr + kwargs_repr)

            # Get class name if available
            class_name = ""
            if args and hasattr(args[0], "__class__"):
                class_name = args[0].__class__.__name__ + "."

            # Log method entry
            func_logger.log(level, f"→ {class_name}{func.__name__}({signature})")

            try:
                result = func(*args, **kwargs)
                # Log method exit (without result to avoid noise)
                func_logger.log(level, f"← {class_name}{func.__name__}() completed")
                return result
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
            except Exception as e:
                # Log exception exit
                func_logger.log(level, f"← {class_name}{func.__name__}() raised {type(e).__name__}: {e}")
                raise

        return wrapper  # type: ignore

    return decorator
