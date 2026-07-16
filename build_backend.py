"""PEP 517 adapter for fbuild's setuptools backend.

Pip cannot append arbitrary Cargo arguments to a PEP 517 build.  It can,
however, pass backend configuration through ``--config-settings``.  Translate
the fbuild-specific profile setting into the environment variable consumed by
``setup.py`` and delegate all other behavior to setuptools.
"""

from __future__ import annotations

import os
from contextlib import contextmanager
from typing import Iterator

from setuptools import build_meta as _setuptools


def _setting(config_settings: dict[str, object] | None, name: str) -> str | None:
    if not config_settings or name not in config_settings:
        return None
    value = config_settings[name]
    if isinstance(value, list):
        value = value[-1] if value else ""
    return str(value).strip().lower()


@contextmanager
def _profile_environment(config_settings: dict[str, object] | None) -> Iterator[None]:
    """Apply the requested fbuild profile only while setuptools builds."""
    profile = _setting(config_settings, "fbuild-profile")
    release = _setting(config_settings, "fbuild-release")
    if profile is not None and profile not in {"dev", "debug", "release"}:
        raise ValueError("fbuild-profile must be 'dev'/'debug' or 'release'")
    if release is not None and release not in {"0", "1", "false", "true", "no", "yes"}:
        raise ValueError("fbuild-release must be a boolean value")

    requested_release = (
        profile == "release"
        if profile is not None
        else release in {"1", "true", "yes"}
        if release is not None
        else None
    )
    if requested_release is None:
        yield
        return

    previous = os.environ.get("FBUILD_BUILD_RELEASE")
    os.environ["FBUILD_BUILD_RELEASE"] = "1" if requested_release else "0"
    try:
        yield
    finally:
        if previous is None:
            os.environ.pop("FBUILD_BUILD_RELEASE", None)
        else:
            os.environ["FBUILD_BUILD_RELEASE"] = previous


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    with _profile_environment(config_settings):
        return _setuptools.build_wheel(wheel_directory, config_settings, metadata_directory)


def build_editable(wheel_directory, config_settings=None, metadata_directory=None):
    with _profile_environment(config_settings):
        return _setuptools.build_editable(wheel_directory, config_settings, metadata_directory)


def prepare_metadata_for_build_wheel(metadata_directory, config_settings=None):
    with _profile_environment(config_settings):
        return _setuptools.prepare_metadata_for_build_wheel(metadata_directory, config_settings)


def get_requires_for_build_wheel(config_settings=None):
    with _profile_environment(config_settings):
        return _setuptools.get_requires_for_build_wheel(config_settings)


def build_sdist(sdist_directory, config_settings=None):
    with _profile_environment(config_settings):
        return _setuptools.build_sdist(sdist_directory, config_settings)


def get_requires_for_build_sdist(config_settings=None):
    with _profile_environment(config_settings):
        return _setuptools.get_requires_for_build_sdist(config_settings)


def prepare_metadata_for_build_editable(metadata_directory, config_settings=None):
    with _profile_environment(config_settings):
        return _setuptools.prepare_metadata_for_build_editable(metadata_directory, config_settings)
