"""
PlatformIO.ini configuration parser.

This module provides functionality to parse platformio.ini files and extract
environment configurations for building embedded projects.
"""

import configparser
from pathlib import Path
from typing import Dict, List, Optional


class PlatformIOConfigError(Exception):
    """Exception raised for platformio.ini configuration errors."""

    pass


class PlatformIOConfig:
    """
    Parser for platformio.ini configuration files.

    This class handles parsing of PlatformIO-style INI files, extracting
    environment configurations, and validating required fields.

    Example platformio.ini:
        [env:uno]
        platform = atmelavr
        board = uno
        framework = arduino

    Usage:
        config = PlatformIOConfig(Path("platformio.ini"))
        envs = config.get_environments()
        uno_config = config.get_env_config("uno")
    """

    REQUIRED_FIELDS = {"platform", "board", "framework"}

    def __init__(self, ini_path: Path):
        """
        Initialize the parser with a platformio.ini file.

        Args:
            ini_path: Path to the platformio.ini file

        Raises:
            PlatformIOConfigError: If the file doesn't exist or cannot be parsed
        """
        self.ini_path = ini_path

        if not ini_path.exists():
            raise PlatformIOConfigError(f"Configuration file not found: {ini_path}")

        self.config = configparser.ConfigParser(
            allow_no_value=True, interpolation=configparser.ExtendedInterpolation()
        )

        try:
            self.config.read(ini_path, encoding="utf-8")
        except configparser.Error as e:
            raise PlatformIOConfigError(f"Failed to parse {ini_path}: {e}") from e

    def get_environments(self) -> List[str]:
        """
        Get list of all environment names defined in the config.

        Returns:
            List of environment names (e.g., ['uno', 'mega', 'nano'])

        Example:
            For [env:uno], [env:mega], returns ['uno', 'mega']
        """
        envs = []
        for section in self.config.sections():
            if section.startswith("env:"):
                env_name = section.split(":", 1)[1]
                envs.append(env_name)
        return envs

    def get_env_config(self, env_name: str) -> Dict[str, str]:
        """
        Get configuration for a specific environment.

        Args:
            env_name: Name of the environment (e.g., 'uno')

        Returns:
            Dictionary of configuration key-value pairs

        Raises:
            PlatformIOConfigError: If environment not found or missing required fields

        Example:
            config.get_env_config('uno')
            # Returns: {'platform': 'atmelavr', 'board': 'uno', 'framework': 'arduino'}
        """
        section = f"env:{env_name}"

        if section not in self.config:
            available = ", ".join(self.get_environments())
            raise PlatformIOConfigError(
                f"Environment '{env_name}' not found. "
                f"Available environments: {available or 'none'}"
            )

        # Collect all key-value pairs from the environment section
        env_config = {}
        for key in self.config[section]:
            value = self.config[section][key]
            # Handle multi-line values (like lib_deps)
            if value is not None:
                env_config[key] = value.strip()
            else:
                env_config[key] = ""

        # Also check if there's a base [env] section to inherit from
        if "env" in self.config:
            base_config = dict(self.config["env"])
            # Environment-specific values override base values
            env_config = {**base_config, **env_config}

        # Validate required fields
        missing_fields = self.REQUIRED_FIELDS - set(env_config.keys())
        if missing_fields:
            raise PlatformIOConfigError(
                f"Environment '{env_name}' is missing required fields: "
                f"{', '.join(sorted(missing_fields))}"
            )

        return env_config

    def get_build_flags(self, env_name: str) -> List[str]:
        """
        Parse and return build flags for an environment.

        Args:
            env_name: Name of the environment

        Returns:
            List of build flags

        Example:
            For build_flags = -DDEBUG -DVERSION=1.0
            Returns: ['-DDEBUG', '-DVERSION=1.0']
        """
        env_config = self.get_env_config(env_name)
        build_flags_str = env_config.get("build_flags", "")

        if not build_flags_str:
            return []

        # Split on whitespace and newlines, filter empty strings
        flags = build_flags_str.split()
        return [flag for flag in flags if flag]

    def get_lib_deps(self, env_name: str) -> List[str]:
        """
        Parse and return library dependencies for an environment.

        Args:
            env_name: Name of the environment

        Returns:
            List of library dependencies

        Example:
            For lib_deps =
                SPI
                Wire
            Returns: ['SPI', 'Wire']
        """
        env_config = self.get_env_config(env_name)
        lib_deps_str = env_config.get("lib_deps", "")

        if not lib_deps_str:
            return []

        # Split on newlines and commas, strip whitespace, filter empty
        deps = []
        for line in lib_deps_str.split("\n"):
            for dep in line.split(","):
                dep = dep.strip()
                if dep:
                    deps.append(dep)
        return deps

    def has_environment(self, env_name: str) -> bool:
        """
        Check if an environment exists in the configuration.

        Args:
            env_name: Name of the environment to check

        Returns:
            True if environment exists, False otherwise
        """
        return f"env:{env_name}" in self.config

    def get_default_environment(self) -> Optional[str]:
        """
        Get the default environment from platformio.ini.

        Returns:
            Default environment name, or first available environment, or None

        Example:
            If [platformio] section has default_envs = uno, returns 'uno'
            Otherwise returns the first environment found
        """
        # Check for explicit default_envs in [platformio] section
        if "platformio" in self.config:
            default_envs = self.config["platformio"].get("default_envs", "").strip()
            if default_envs:
                # Can be comma-separated, take the first one
                return default_envs.split(",")[0].strip()

        # Fall back to first environment
        envs = self.get_environments()
        return envs[0] if envs else None
