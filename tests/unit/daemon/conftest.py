"""Pytest configuration for daemon unit tests."""

import sys

import pytest


@pytest.fixture(autouse=True)
def isolate_output_globals():
    """Reset output.py context and global state before/after each test.

    Prevents cross-test contamination of module-level globals and contextvars.
    This fixture automatically runs for every test in the daemon directory
    to ensure test isolation even when tests run in parallel.

    Now that output.py uses contextvars, we reset both the context and
    the deprecated globals for backward compatibility.
    """
    from fbuild import output

    # Save original context
    original_ctx = output.get_context()

    # Save original deprecated globals
    original_start_time = output._start_time
    original_output_stream = output._output_stream
    original_verbose = output._verbose
    original_output_file = output._output_file

    # Reset context to default for this test
    output._output_context.set(
        output.OutputContext(
            start_time=None,
            output_stream=sys.stdout,
            verbose=True,
            output_file=None,
        )
    )

    # Reset deprecated globals for backward compatibility
    output._start_time = None
    output._output_stream = sys.stdout
    output._verbose = True
    output._output_file = None

    yield

    # Restore original context
    output._output_context.set(original_ctx)

    # Restore original deprecated globals
    output._start_time = original_start_time
    output._output_stream = original_output_stream
    output._verbose = original_verbose
    output._output_file = original_output_file

    # Close file if test left it open
    ctx = output.get_context()
    if ctx.output_file and not ctx.output_file.closed:
        ctx.output_file.close()
