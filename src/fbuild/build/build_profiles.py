"""Build Profile Configuration.

This module defines build profiles that modify platform JSON flags.

Design:
    Platform JSON files are the source of truth for all flags. Base compiler and
    linker flags do NOT include optimization or LTO flags. Instead, each profile
    adds its own optimization/LTO flags:

    - RELEASE: Adds -Os and LTO flags for optimized builds
    - QUICK: Adds -O2 without LTO for faster compilation

    Profile configuration in JSON (profiles.{profile}):
    - compile_flags: Optimization and LTO flags for compilation
    - link_flags: LTO flags for linking

    The system:
    1. Reads base flags from platform JSON configuration (no optimization/LTO)
    2. Adds profile-specific flags from JSON (compile_flags, link_flags)

    This is simpler than the previous filter-based approach - no regex needed.
"""

from dataclasses import dataclass
from enum import Enum
from typing import List


class BuildProfile(Enum):
    """Build profile enum for type-safe profile selection."""

    RELEASE = "release"
    QUICK = "quick"

    def __str__(self) -> str:
        """Return the string value for directory names and display."""
        return self.value


@dataclass(frozen=True)
class ProfileFlags:
    """Build profile configuration.

    Attributes:
        name: Profile identifier (matches BuildProfile enum value)
        description: Human-readable profile description
        compile_flags: Flags to ADD (loaded from JSON)
        link_flags: Flags to ADD (loaded from JSON)
        controlled_patterns: DEPRECATED - kept for backward compatibility, always empty
    """

    name: str
    description: str
    compile_flags: tuple[str, ...]
    link_flags: tuple[str, ...]
    controlled_patterns: tuple[str, ...]  # DEPRECATED - always empty


# Profile configurations - keyed by BuildProfile enum string value
PROFILES: dict[str, ProfileFlags] = {
    "release": ProfileFlags(
        name="release",
        description="Optimized release build with LTO",
        compile_flags=(),  # Loaded from JSON config profiles.release.compile_flags
        link_flags=(),  # Loaded from JSON config profiles.release.link_flags
        controlled_patterns=(),  # DEPRECATED
    ),
    "quick": ProfileFlags(
        name="quick",
        description="Fast development build (no LTO, -O2)",
        compile_flags=(),  # Loaded from JSON config profiles.quick.compile_flags
        link_flags=(),  # Loaded from JSON config profiles.quick.link_flags
        controlled_patterns=(),  # DEPRECATED
    ),
}


def get_profile(profile: BuildProfile) -> ProfileFlags:
    """Get profile configuration by enum.

    Args:
        profile: BuildProfile enum value

    Returns:
        ProfileFlags for the requested profile
    """
    return PROFILES[profile.value]


def get_profile_flags_from_config(
    profile: BuildProfile,
    platform_config: dict
) -> tuple[tuple[str, ...], tuple[str, ...]]:
    """Load profile-specific flags from platform JSON config.

    This function extracts compile and link flags from the platform's JSON
    configuration file based on the selected build profile.

    Args:
        profile: BuildProfile enum value
        platform_config: Platform configuration dictionary (loaded from JSON)

    Returns:
        Tuple of (compile_flags, link_flags) as tuples of strings
    """
    profiles = platform_config.get("profiles", {})
    profile_data = profiles.get(profile.value, {})
    compile_flags = tuple(profile_data.get("compile_flags", []))
    link_flags = tuple(profile_data.get("link_flags", []))
    return compile_flags, link_flags


# Legacy functions kept for backward compatibility
# These no longer do any filtering since controlled_patterns is always empty


def filter_platform_flags(flags: List[str], profile_flags: ProfileFlags, profile: BuildProfile | None = None) -> List[str]:
    """DEPRECATED: Returns flags unchanged. Filtering removed in favor of explicit profile flags."""
    return flags


def merge_compile_flags(platform_flags: List[str], profile_flags: ProfileFlags) -> List[str]:
    """DEPRECATED: Returns platform_flags unchanged. Use get_profile_flags_from_config instead."""
    return platform_flags


def merge_link_flags(platform_flags: List[str], profile_flags: ProfileFlags) -> List[str]:
    """DEPRECATED: Returns platform_flags unchanged. Use get_profile_flags_from_config instead."""
    return platform_flags


def get_compile_flags(profile: BuildProfile, base_flags: List[str] | None = None) -> List[str]:
    """DEPRECATED: Returns base_flags unchanged. Use get_profile_flags_from_config instead."""
    return list(base_flags) if base_flags else []


def get_link_flags(profile: BuildProfile, base_flags: List[str] | None = None) -> List[str]:
    """DEPRECATED: Returns base_flags unchanged. Use get_profile_flags_from_config instead."""
    return list(base_flags) if base_flags else []


def format_profile_banner(profile: BuildProfile, compiler: str | None = None) -> str:
    """Format a build profile banner for display.

    Args:
        profile: BuildProfile enum value
        compiler: Compiler name and version (optional)

    Returns:
        Formatted banner string
    """
    parts = [f"PROFILE={profile.value}"]
    if compiler:
        parts.append(f"COMPILER={compiler}")

    return " ".join(parts)


def print_profile_banner(profile: BuildProfile, compiler: str | None = None) -> None:
    """Print the build profile banner to the console.

    Uses the fbuild output module for consistent formatting.

    Args:
        profile: BuildProfile enum value
        compiler: Compiler name and version (optional)
    """
    from ..output import log

    banner = format_profile_banner(profile, compiler=compiler)
    log(banner)
