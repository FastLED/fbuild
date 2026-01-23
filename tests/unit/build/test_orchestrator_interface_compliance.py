"""
Unit tests for orchestrator interface compliance.

This module verifies that all platform orchestrator implementations
comply with the IBuildOrchestrator interface and PlatformBuildMethod protocol.

Tests cover:
- Build method existence and signature validation
- Parameter types and defaults
- Internal build method signatures
- Context manager usage (managed_compilation_queue)
- Runtime parameter acceptance
"""

import inspect
from pathlib import Path
import pytest

from fbuild.build.orchestrator import IBuildOrchestrator, BuildResult
from fbuild.build.orchestrator_avr import BuildOrchestratorAVR
from fbuild.build.orchestrator_esp32 import OrchestratorESP32
from fbuild.build.orchestrator_teensy import OrchestratorTeensy
from fbuild.build.orchestrator_rp2040 import OrchestratorRP2040
from fbuild.build.orchestrator_stm32 import OrchestratorSTM32


# List of all orchestrator classes to test
ALL_ORCHESTRATORS = [
    BuildOrchestratorAVR,
    OrchestratorESP32,
    OrchestratorTeensy,
    OrchestratorRP2040,
    OrchestratorSTM32,
]


class TestOrchestratorInterface:
    """Test that all orchestrators implement the IBuildOrchestrator interface."""

    def test_all_orchestrators_have_build_method(self):
        """Verify all orchestrators have a build() method."""
        for orchestrator_class in ALL_ORCHESTRATORS:
            assert hasattr(orchestrator_class, "build"), f"{orchestrator_class.__name__} missing build() method"
            assert callable(getattr(orchestrator_class, "build")), f"{orchestrator_class.__name__}.build is not callable"

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_build_method_signature(self, orchestrator_class):
        """Verify build() method has correct signature with required parameters."""
        build_method = getattr(orchestrator_class, "build")
        sig = inspect.signature(build_method)

        # Check that all required parameters exist
        params = sig.parameters

        # self is always first for instance methods
        assert "self" in params, f"{orchestrator_class.__name__}.build() missing 'self' parameter"

        # Required positional parameter: project_dir
        assert "project_dir" in params, f"{orchestrator_class.__name__}.build() missing 'project_dir' parameter"
        project_dir_param = params["project_dir"]
        # Should accept Path type
        assert project_dir_param.annotation in [Path, "Path"], f"{orchestrator_class.__name__}.build() project_dir should be Path type"

        # Optional parameters with defaults
        assert "env_name" in params, f"{orchestrator_class.__name__}.build() missing 'env_name' parameter"
        env_name_param = params["env_name"]
        assert env_name_param.default is not inspect.Parameter.empty or env_name_param.default is None, (
            f"{orchestrator_class.__name__}.build() env_name should have default value"
        )

        assert "clean" in params, f"{orchestrator_class.__name__}.build() missing 'clean' parameter"
        clean_param = params["clean"]
        assert clean_param.default is False, f"{orchestrator_class.__name__}.build() clean default should be False, got {clean_param.default}"

        assert "verbose" in params, f"{orchestrator_class.__name__}.build() missing 'verbose' parameter"
        verbose_param = params["verbose"]
        assert verbose_param.default is not inspect.Parameter.empty, f"{orchestrator_class.__name__}.build() verbose should have default value"

        # Critical: jobs parameter for parallel compilation
        assert "jobs" in params, f"{orchestrator_class.__name__}.build() missing 'jobs' parameter"
        jobs_param = params["jobs"]
        assert jobs_param.default is None or jobs_param.default is inspect.Parameter.empty, (
            f"{orchestrator_class.__name__}.build() jobs default should be None, got {jobs_param.default}"
        )

        # Verify return type is BuildResult
        assert sig.return_annotation in [BuildResult, "BuildResult"], (
            f"{orchestrator_class.__name__}.build() should return BuildResult, got {sig.return_annotation}"
        )

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_internal_build_methods_have_jobs_parameter(self, orchestrator_class):
        """Verify internal _build_XXX() methods accept jobs parameter."""
        # Find all internal build methods (_build_*)
        internal_build_methods = [
            name
            for name in dir(orchestrator_class)
            if name.startswith("_build_") and callable(getattr(orchestrator_class, name))
        ]

        # Should have at least one internal build method
        if internal_build_methods:
            for method_name in internal_build_methods:
                method = getattr(orchestrator_class, method_name)
                sig = inspect.signature(method)
                params = sig.parameters

                # Check if jobs parameter exists
                assert "jobs" in params, (
                    f"{orchestrator_class.__name__}.{method_name}() missing 'jobs' parameter. "
                    f"Internal build methods should accept jobs parameter for parallel compilation."
                )

                # Verify jobs parameter has correct default (None)
                jobs_param = params["jobs"]
                assert jobs_param.default is None or jobs_param.default is inspect.Parameter.empty, (
                    f"{orchestrator_class.__name__}.{method_name}() jobs default should be None, "
                    f"got {jobs_param.default}"
                )

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_context_manager_usage(self, orchestrator_class):
        """Verify orchestrators use managed_compilation_queue context manager."""
        import textwrap

        # Get all methods in the orchestrator
        all_methods = [
            (name, getattr(orchestrator_class, name))
            for name in dir(orchestrator_class)
            if callable(getattr(orchestrator_class, name))
            and (name == "build" or name.startswith("_build_"))
        ]

        # Check if any of the build methods use managed_compilation_queue
        uses_context_manager = False
        has_manual_cleanup = []

        for method_name, method in all_methods:
            try:
                source = inspect.getsource(method)
                source = textwrap.dedent(source)

                # Check for managed_compilation_queue usage
                if "managed_compilation_queue" in source:
                    uses_context_manager = True

                    # Verify it's used as a context manager (with statement)
                    assert "with managed_compilation_queue" in source, (
                        f"{orchestrator_class.__name__}.{method_name}() should use "
                        f"'with managed_compilation_queue' pattern. "
                        f"Found managed_compilation_queue but not used as context manager."
                    )

                # Check for manual cleanup code
                manual_cleanup_patterns = [
                    "compilation_queue.shutdown()",
                    "queue.shutdown()",
                    "compilation_queue.shutdown_and_wait()",
                    "queue.shutdown_and_wait()",
                ]
                for pattern in manual_cleanup_patterns:
                    if pattern in source:
                        has_manual_cleanup.append((method_name, pattern))
            except (OSError, TypeError):
                # Built-in methods or methods we can't inspect
                continue

        # At least one method should use managed_compilation_queue
        assert uses_context_manager, (
            f"{orchestrator_class.__name__} should use managed_compilation_queue context manager "
            f"in build() or _build_XXX() methods. This ensures proper cleanup of temporary compilation queues."
        )

        # No method should have manual cleanup
        assert not has_manual_cleanup, (
            f"{orchestrator_class.__name__} contains manual cleanup code: {has_manual_cleanup}. "
            f"Use managed_compilation_queue context manager instead for automatic cleanup."
        )

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_runtime_parameter_acceptance(self, orchestrator_class):
        """Verify orchestrators can be instantiated and accept jobs parameter."""
        # Try to instantiate the orchestrator
        # Most orchestrators accept cache and verbose in __init__
        try:
            orchestrator = orchestrator_class(cache=None, verbose=False)
        except TypeError:
            # Some might have different __init__ signatures
            try:
                orchestrator = orchestrator_class()
            except Exception as e:
                pytest.fail(f"Failed to instantiate {orchestrator_class.__name__}: {e}")

        # Verify the build method signature accepts jobs parameter
        build_method = getattr(orchestrator, "build")
        sig = inspect.signature(build_method)

        # Create a minimal call with jobs parameter
        # This should not raise TypeError about unexpected keyword argument
        try:
            # We're not actually calling it, just verifying the signature
            bound_args = sig.bind(
                project_dir=Path("/fake/path"),
                env_name="test",
                clean=False,
                verbose=False,
                jobs=4,  # Critical: jobs parameter
            )
            # If we get here, binding succeeded - jobs parameter is accepted
            assert "jobs" in bound_args.arguments, "jobs parameter not bound correctly"
        except TypeError as e:
            pytest.fail(
                f"{orchestrator_class.__name__}.build() failed to bind jobs parameter: {e}. "
                f"Signature: {sig}"
            )


class TestOrchestratorInheritance:
    """Test that all orchestrators properly inherit from IBuildOrchestrator."""

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_orchestrator_inherits_interface(self, orchestrator_class):
        """Verify orchestrator inherits from IBuildOrchestrator."""
        assert issubclass(orchestrator_class, IBuildOrchestrator), (
            f"{orchestrator_class.__name__} must inherit from IBuildOrchestrator"
        )

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_orchestrator_implements_abstract_methods(self, orchestrator_class):
        """Verify orchestrator implements all abstract methods from interface."""
        # Get all abstract methods from the interface
        abstract_methods = {
            name
            for name, method in inspect.getmembers(IBuildOrchestrator, predicate=inspect.isfunction)
            if getattr(method, "__isabstractmethod__", False)
        }

        # Verify the orchestrator implements all of them
        for method_name in abstract_methods:
            assert hasattr(orchestrator_class, method_name), (
                f"{orchestrator_class.__name__} missing implementation of abstract method {method_name}"
            )
            method = getattr(orchestrator_class, method_name)
            assert callable(method), f"{orchestrator_class.__name__}.{method_name} is not callable"

            # Verify it's not still abstract
            assert not getattr(method, "__isabstractmethod__", False), (
                f"{orchestrator_class.__name__}.{method_name} is still abstract (not implemented)"
            )


class TestOrchestratorConsistency:
    """Test consistency across all orchestrator implementations."""

    def test_all_orchestrators_have_consistent_parameter_order(self):
        """Verify all orchestrators have the same parameter order in build()."""
        # Get parameter names from each orchestrator
        param_orders = {}
        for orchestrator_class in ALL_ORCHESTRATORS:
            build_method = getattr(orchestrator_class, "build")
            sig = inspect.signature(build_method)
            # Exclude 'self'
            param_names = [name for name in sig.parameters.keys() if name != "self"]
            param_orders[orchestrator_class.__name__] = param_names

        # All should have the same order
        reference_order = param_orders[ALL_ORCHESTRATORS[0].__name__]
        for orchestrator_name, param_names in param_orders.items():
            assert param_names == reference_order, (
                f"{orchestrator_name}.build() has inconsistent parameter order. "
                f"Expected: {reference_order}, Got: {param_names}"
            )

    def test_all_orchestrators_have_consistent_defaults(self):
        """Verify all orchestrators have the same default values."""
        # Expected defaults for each parameter (excluding self)
        expected_defaults = {
            "project_dir": inspect.Parameter.empty,  # Required, no default
            "env_name": None,  # Optional
            "clean": False,
            "verbose": None,  # Optional[bool]
            "jobs": None,  # Optional[int]
        }

        for orchestrator_class in ALL_ORCHESTRATORS:
            build_method = getattr(orchestrator_class, "build")
            sig = inspect.signature(build_method)

            for param_name, expected_default in expected_defaults.items():
                param = sig.parameters.get(param_name)
                assert param is not None, f"{orchestrator_class.__name__}.build() missing parameter {param_name}"

                actual_default = param.default
                assert actual_default == expected_default, (
                    f"{orchestrator_class.__name__}.build() parameter '{param_name}' has wrong default. "
                    f"Expected: {expected_default}, Got: {actual_default}"
                )


class TestOrchestratorDocumentation:
    """Test that orchestrators have proper documentation."""

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_orchestrator_has_docstring(self, orchestrator_class):
        """Verify orchestrator class has a docstring."""
        assert orchestrator_class.__doc__ is not None, f"{orchestrator_class.__name__} missing class docstring"
        assert len(orchestrator_class.__doc__.strip()) > 0, f"{orchestrator_class.__name__} has empty docstring"

    @pytest.mark.parametrize("orchestrator_class", ALL_ORCHESTRATORS, ids=lambda cls: cls.__name__)
    def test_build_method_has_docstring(self, orchestrator_class):
        """Verify build() method has a docstring."""
        build_method = getattr(orchestrator_class, "build")
        assert build_method.__doc__ is not None, f"{orchestrator_class.__name__}.build() missing docstring"
        assert len(build_method.__doc__.strip()) > 0, f"{orchestrator_class.__name__}.build() has empty docstring"
