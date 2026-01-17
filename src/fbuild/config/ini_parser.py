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

        # Use interpolation=None to disable Python's built-in interpolation.
        # PlatformIO uses a different syntax: ${env:section.key} instead of ${section:key}
        # We handle this manually in get_env_config() with a custom regex-based interpolation.
        self.config = configparser.ConfigParser(allow_no_value=True, interpolation=None)

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

    def get_env_config(self, env_name: str, _visited: Optional[set] = None, _validate: bool = True) -> Dict[str, str]:
        """
        Get configuration for a specific environment with inheritance support.

        Args:
            env_name: Name of the environment (e.g., 'uno')
            _visited: Internal parameter for circular dependency detection
            _validate: Internal parameter to control validation (default: True)

        Returns:
            Dictionary of configuration key-value pairs

        Raises:
            PlatformIOConfigError: If environment not found, missing required fields,
                                  or circular dependency detected

        Example:
            config.get_env_config('uno')
            # Returns: {'platform': 'atmelavr', 'board': 'uno', 'framework': 'arduino'}
        """
        # Initialize visited set for circular dependency detection
        if _visited is None:
            _visited = set()

        # Check for circular dependency
        if env_name in _visited:
            chain = " -> ".join(_visited) + f" -> {env_name}"
            raise PlatformIOConfigError(f"Circular dependency detected in environment inheritance: {chain}")

        _visited.add(env_name)

        section = f"env:{env_name}"

        if section not in self.config:
            available = ", ".join(self.get_environments())
            raise PlatformIOConfigError(f"Environment '{env_name}' not found. " + f"Available environments: {available or 'none'}")

        # Collect all key-value pairs from the environment section
        # Use raw=True to avoid interpolation errors when referencing parent environments
        env_config = {}
        for key in self.config[section]:
            value = self.config[section].get(key, raw=True)
            # Handle multi-line values (like lib_deps)
            env_config[key] = value.strip() if value else ""

        # Also check if there's a base [env] section to inherit from
        if "env" in self.config:
            base_config = {}
            for key in self.config["env"]:
                value = self.config["env"].get(key, raw=True)
                base_config[key] = value.strip() if value else ""
            # Environment-specific values override base values
            env_config = {**base_config, **env_config}

        # Handle 'extends' directive for environment inheritance
        if "extends" in env_config:
            parent_ref = env_config["extends"].strip()
            # Parse parent reference (can be "env:parent" or just "parent")
            parent_name = parent_ref.replace("env:", "").strip()

            # Recursively get parent config (don't validate parent, it might be abstract)
            parent_config = self.get_env_config(parent_name, _visited, _validate=False)

            # Merge: parent values first, then child overrides
            merged_config = parent_config.copy()
            for key, value in env_config.items():
                if key == "extends":
                    # Remove 'extends' key from final config
                    continue
                merged_config[key] = value

            env_config = merged_config

        # Now perform manual variable interpolation for cross-environment references
        # This handles ${env:parent.key} syntax
        import re

        interpolated_config = {}
        for key, value in env_config.items():
            # Look for ${env:name.key} patterns
            pattern = r"\$\{env:([^}]+)\}"
            matches = re.findall(pattern, value)

            interpolated_value = value
            for match in matches:
                # Parse the reference: "env:parent.build_flags"
                parts = match.split(".")
                if len(parts) == 2:
                    ref_env = parts[0]
                    ref_key = parts[1]

                    # Get the referenced environment's config
                    # Create a new visited set to avoid false circular dependency detection
                    # Don't validate the referenced environment (it might be an abstract base)
                    ref_config = self.get_env_config(ref_env, set(), _validate=False)

                    if ref_key in ref_config:
                        # Replace the variable reference with the actual value
                        interpolated_value = interpolated_value.replace(f"${{env:{match}}}", ref_config[ref_key])

            interpolated_config[key] = interpolated_value

        env_config = interpolated_config

        # Validate required fields (only if validation is enabled)
        if _validate:
            missing_fields = self.REQUIRED_FIELDS - set(env_config.keys())
            if missing_fields:
                raise PlatformIOConfigError(f"Environment '{env_name}' is missing required fields: " + f"{', '.join(sorted(missing_fields))}")

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

    def get_src_dir(self) -> Optional[str]:
        """
        Get source directory override from [platformio] section.

        Returns:
            Source directory path relative to project root, or None if not specified

        Example:
            If [platformio] section has src_dir = examples/Blink, returns 'examples/Blink'
        """
        if "platformio" in self.config:
            src_dir = self.config["platformio"].get("src_dir", "").strip()
            # Remove inline comments (everything after ';')
            if ";" in src_dir:
                src_dir = src_dir.split(";")[0].strip()
            return src_dir if src_dir else None
        return None

    def get_platformio_config(self, key: str, default: Optional[str] = None) -> Optional[str]:
        """
        Get a configuration value from the [platformio] section.

        Args:
            key: Configuration key to retrieve
            default: Default value if key not found

        Returns:
            Configuration value or default

        Example:
            build_cache_dir = config.get_platformio_config('build_cache_dir', '.pio/build_cache')
        """
        if "platformio" in self.config:
            value = self.config["platformio"].get(key, "").strip()
            return value if value else default
        return default

    def get_board_overrides(self, env_name: str) -> Dict[str, str]:
        """
        Get board build and upload overrides from environment configuration.

        Extracts all board_build.* and board_upload.* settings from the
        environment configuration.

        Args:
            env_name: Name of the environment

        Returns:
            Dictionary of board override settings (e.g., {'flash_mode': 'dio', 'flash_size': '4MB'})

        Example:
            For board_build.flash_mode = dio and board_build.flash_size = 4MB
            Returns: {'flash_mode': 'dio', 'flash_size': '4MB'}
        """
        env_config = self.get_env_config(env_name)
        overrides = {}

        for key, value in env_config.items():
            if key.startswith("board_build."):
                # Extract the override key (e.g., "flash_mode" from "board_build.flash_mode")
                override_key = key.replace("board_build.", "")
                overrides[override_key] = value
            elif key.startswith("board_upload."):
                # Also handle upload overrides
                override_key = "upload_" + key.replace("board_upload.", "")
                overrides[override_key] = value

        return overrides
