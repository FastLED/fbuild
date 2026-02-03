"""Build Profile Configuration.

This module defines generic, profile-agnostic compilation and linking flags.

Design:
    Profiles declare ALL flags they control explicitly as generic compile_flags
    and link_flags. fbuild doesn't know or care what those flags are (LTO,
    section flags, etc.) - the specific flags are implementation details of
    each profile.

    The system:
    1. Filters out controlled flags from platform configuration
    2. Merges in profile-specific flags
    3. This is declarative - no ad-hoc flag manipulation elsewhere
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
    """Generic build profile flags.

    Profiles declare all flags they control explicitly. fbuild code just uses
    profile.compile_flags and profile.link_flags without knowing what's in them.

    All fields are mandatory - no defaults.

    Attributes:
        name: Profile identifier (matches BuildProfile enum value)
        description: Human-readable profile description
        compile_flags: All compilation flags for this profile
        link_flags: All linker flags for this profile
        controlled_patterns: Flag prefixes this profile controls (stripped from platform config)
    """

    name: str
    description: str
    compile_flags: tuple[str, ...]
    link_flags: tuple[str, ...]
    controlled_patterns: tuple[str, ...]  # Ordered tuple, not unordered set


# Profile configurations - keyed by BuildProfile enum
PROFILES: dict[BuildProfile, ProfileFlags] = {
    BuildProfile.RELEASE: ProfileFlags(
        name="release",
        description="Optimized release build with LTO (default)",
        compile_flags=(
            "-Os",
            "-ffunction-sections",
            "-fdata-sections",
            "-flto",
            "-fno-fat-lto-objects",
        ),
        link_flags=(
            "-Wl,--gc-sections",
            "-flto",
            "-fuse-linker-plugin",
        ),
        controlled_patterns=(
            "-O",
            "-flto",
            "-fno-fat-lto-objects",
            "-fuse-linker-plugin",
            "-ffunction-sections",
            "-fdata-sections",
            "-Wl,--gc-sections",
        ),
    ),
    BuildProfile.QUICK: ProfileFlags(
        name="quick",
        description="Fast development build (no LTO)",
        compile_flags=(
            "-O2",
            "-ffunction-sections",
            "-fdata-sections",
        ),
        link_flags=(
            "-Wl,--gc-sections",
        ),
        controlled_patterns=(
            "-O",
            "-flto",
            "-fno-fat-lto-objects",
            "-fuse-linker-plugin",
            "-ffunction-sections",
            "-fdata-sections",
            "-Wl,--gc-sections",
        ),
    ),
}


def filter_platform_flags(flags: List[str], profile_flags: ProfileFlags) -> List[str]:
    """Remove flags that the profile controls from platform config.

    This strips any flags matching the profile's controlled_patterns so that
    the profile's explicit flags take precedence.

    Args:
        flags: Platform configuration flags
        profile_flags: The profile flags whose controlled patterns to filter

    Returns:
        Filtered list of flags with controlled patterns removed
    """
    return [f for f in flags if not any(f.startswith(p) for p in profile_flags.controlled_patterns)]


def merge_compile_flags(platform_flags: List[str], profile_flags: ProfileFlags) -> List[str]:
    """Merge platform flags with profile compile flags.

    Filters out profile-controlled flags from platform config, then appends
    all profile compile flags.

    Args:
        platform_flags: Flags from platform configuration
        profile_flags: The profile flags to apply

    Returns:
        Merged list of compile flags
    """
    return filter_platform_flags(platform_flags, profile_flags) + list(profile_flags.compile_flags)


def merge_link_flags(platform_flags: List[str], profile_flags: ProfileFlags) -> List[str]:
    """Merge platform flags with profile link flags.

    Filters out profile-controlled flags from platform config, then appends
    all profile link flags.

    Args:
        platform_flags: Flags from platform configuration
        profile_flags: The profile flags to apply

    Returns:
        Merged list of link flags
    """
    return filter_platform_flags(platform_flags, profile_flags) + list(profile_flags.link_flags)


def get_profile(profile: BuildProfile) -> ProfileFlags:
    """Get profile configuration by enum.

    Args:
        profile: BuildProfile enum value

    Returns:
        ProfileFlags for the requested profile
    """
    return PROFILES[profile]


def get_compile_flags(profile: BuildProfile, base_flags: List[str] | None = None) -> List[str]:
    """Get compilation flags for a profile.

    Filters controlled flags from base_flags and appends profile's compile_flags.

    Args:
        profile: BuildProfile enum value
        base_flags: Existing compilation flags to filter

    Returns:
        List of flags with profile-specific flags applied
    """
    profile_flags = get_profile(profile)
    flags = list(base_flags) if base_flags else []
    return filter_platform_flags(flags, profile_flags) + list(profile_flags.compile_flags)


def get_link_flags(profile: BuildProfile, base_flags: List[str] | None = None) -> List[str]:
    """Get linker flags for a profile.

    Filters controlled flags from base_flags and appends profile's link_flags.

    Args:
        profile: BuildProfile enum value
        base_flags: Existing linker flags to filter

    Returns:
        List of flags with profile-specific flags applied
    """
    profile_flags = get_profile(profile)
    flags = list(base_flags) if base_flags else []
    return filter_platform_flags(flags, profile_flags) + list(profile_flags.link_flags)


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
