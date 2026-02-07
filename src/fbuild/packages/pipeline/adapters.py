"""Adapters for building PackageTask dependency graphs from platformio.ini configs.

Translates platform-specific package knowledge into generic PackageTask objects
for the parallel pipeline. Each platform (AVR, ESP32, etc.) has its own adapter
function that reads the project configuration and returns a list of PackageTask
instances with proper dependency edges.

Adapter functions:
- build_avr_task_graph(): AVR platform (Arduino Uno, Mega, Nano, etc.)
- build_task_graph(): Auto-detects platform and delegates to the right adapter.

The returned task graphs encode the dependency DAG:
  toolchain-atmelavr (no deps) ─┐
                                 ├──► libraries (depend on framework)
  framework-arduino-avr (no deps)─┘
"""

from pathlib import Path
from typing import Optional

from fbuild.config.ini_parser import PlatformIOConfig
from fbuild.packages.arduino_core import ArduinoCore
from fbuild.packages.cache import Cache
from fbuild.packages.toolchain import ToolchainAVR

from .models import PackageTask


class TaskGraphError(Exception):
    """Raised when building a task graph fails."""

    pass


def _detect_avr_package_filename() -> tuple[str, Optional[str]]:
    """Detect the AVR toolchain package filename and checksum for the current platform.

    Returns:
        Tuple of (download_url, checksum_or_none).

    Raises:
        TaskGraphError: If the current platform is not supported.
    """
    try:
        from fbuild.packages.platform_utils import PlatformDetector

        plat, arch = PlatformDetector.detect_avr_platform()
    except KeyboardInterrupt:
        raise
    except Exception as e:
        raise TaskGraphError(f"Cannot detect platform for AVR toolchain: {e}")

    packages = ToolchainAVR.PACKAGES
    if plat not in packages:
        raise TaskGraphError(f"No AVR toolchain package for platform: {plat}")

    platform_packages = packages[plat]

    # For Windows and macOS, only x86_64 is available
    if plat in ("windows", "darwin"):
        arch = "x86_64"

    if arch not in platform_packages:
        if "x86_64" in platform_packages:
            arch = "x86_64"
        else:
            raise TaskGraphError(f"No AVR toolchain for {plat}/{arch}. Available: {list(platform_packages.keys())}")

    package_filename = platform_packages[arch]
    checksum = platform_packages.get("checksum")

    download_url = f"{ToolchainAVR.BASE_URL}/{package_filename}"
    return download_url, checksum


def build_avr_task_graph(
    project_path: Path,
    env_name: str,
    cache: Cache,
) -> list[PackageTask]:
    """Build a PackageTask dependency graph for an AVR project.

    Reads platformio.ini to determine required packages and constructs
    a dependency DAG with proper ordering:
    - toolchain-atmelavr: AVR-GCC compiler (no dependencies)
    - framework-arduino-avr: Arduino core (no dependencies)
    - Libraries: Each depends on framework-arduino-avr

    Args:
        project_path: Path to the project directory containing platformio.ini.
        env_name: Environment name from platformio.ini (e.g. "uno").
        cache: Cache instance for determining destination paths.

    Returns:
        List of PackageTask instances forming the dependency graph.

    Raises:
        TaskGraphError: If the project config is invalid or platform is unsupported.
    """
    tasks: list[PackageTask] = []

    # Parse platformio.ini
    ini_path = project_path / "platformio.ini"
    try:
        config = PlatformIOConfig(ini_path)
        env_config = config.get_env_config(env_name)
    except KeyboardInterrupt:
        raise
    except Exception as e:
        raise TaskGraphError(f"Failed to parse {ini_path}: {e}")

    platform = env_config.get("platform", "")
    if platform != "atmelavr":
        raise TaskGraphError(f"Expected platform 'atmelavr', got '{platform}'")

    # --- Toolchain task ---
    toolchain_url, _checksum = _detect_avr_package_filename()
    toolchain_dest = str(cache.get_toolchain_path(ToolchainAVR.BASE_URL, ToolchainAVR.VERSION))

    tasks.append(
        PackageTask(
            name="toolchain-atmelavr",
            url=toolchain_url,
            version=ToolchainAVR.VERSION,
            dest_path=toolchain_dest,
            dependencies=[],
        )
    )

    # --- Framework task ---
    framework_url = ArduinoCore.AVR_URL
    framework_dest = str(cache.get_platform_path(ArduinoCore.AVR_URL, ArduinoCore.AVR_VERSION))

    tasks.append(
        PackageTask(
            name="framework-arduino-avr",
            url=framework_url,
            version=ArduinoCore.AVR_VERSION,
            dest_path=framework_dest,
            dependencies=[],
        )
    )

    # --- Library tasks ---
    try:
        lib_deps = config.get_lib_deps(env_name)
    except KeyboardInterrupt:
        raise
    except Exception:
        lib_deps = []

    for lib_spec in lib_deps:
        lib_spec = lib_spec.strip()
        if not lib_spec:
            continue

        lib_name, lib_url, lib_version = _parse_lib_spec(lib_spec)

        # Libraries go into the build directory's libs folder
        lib_dest = str(cache.cache_root / "libraries" / Cache.hash_url(lib_url) / lib_version)

        tasks.append(
            PackageTask(
                name=lib_name,
                url=lib_url,
                version=lib_version,
                dest_path=lib_dest,
                dependencies=["framework-arduino-avr"],
            )
        )

    return tasks


def _parse_lib_spec(lib_spec: str) -> tuple[str, str, str]:
    """Parse a library dependency specification into name, url, version.

    Supports formats:
    - Full URL: https://github.com/user/repo or https://github.com/user/repo.git
    - GitHub shorthand with version: user/repo@version
    - Registry spec: owner/name@^version
    - Simple name: LibName (assumed to be Arduino built-in, uses placeholder)

    Args:
        lib_spec: Library dependency specification string.

    Returns:
        Tuple of (name, url, version).
    """
    lib_spec = lib_spec.strip()

    # Full URL
    if lib_spec.startswith("http://") or lib_spec.startswith("https://"):
        url = lib_spec
        # Extract name from URL path
        path_parts = url.rstrip("/").split("/")
        name = path_parts[-1].replace(".git", "")

        # Try to extract version from URL (GitHub archive URLs)
        version = "latest"
        if "/archive/" in url or "/releases/" in url:
            for part in reversed(path_parts):
                if part and part[0] in "0123456789v":
                    version = part.lstrip("v").replace(".tar.gz", "").replace(".zip", "")
                    break

        return name, url, version

    # Owner/name@version (GitHub shorthand or registry)
    if "@" in lib_spec:
        parts = lib_spec.split("@", 1)
        name_part = parts[0].strip()
        version = parts[1].strip().lstrip("^~>=<!")

        if "/" in name_part:
            # owner/name format
            owner, name = name_part.rsplit("/", 1)
            url = f"https://github.com/{owner}/{name}/archive/refs/tags/v{version}.tar.gz"
        else:
            name = name_part
            url = f"https://registry.platformio.org/packages/{name}"

        return name, url, version

    # Symlink or file protocol
    if lib_spec.startswith("symlink://") or lib_spec.startswith("file://"):
        name = Path(lib_spec.split("://", 1)[1]).name
        return name, lib_spec, "local"

    # Relative path
    if lib_spec.startswith("../") or lib_spec.startswith("./"):
        name = Path(lib_spec).name
        return name, lib_spec, "local"

    # Simple name (built-in Arduino library or registry name)
    if "/" in lib_spec:
        # owner/name without version
        parts = lib_spec.split("/", 1)
        name = parts[1]
        url = f"https://github.com/{lib_spec}"
        return name, url, "latest"

    # Plain library name (e.g., "SPI", "Wire")
    name = lib_spec
    url = f"https://registry.platformio.org/packages/{name}"
    return name, url, "latest"


def _detect_platform(env_config: dict[str, str]) -> str:
    """Detect the platform type from environment config.

    Args:
        env_config: Environment configuration dict from PlatformIOConfig.

    Returns:
        Platform identifier string (e.g. "atmelavr", "espressif32").

    Raises:
        TaskGraphError: If platform cannot be determined.
    """
    platform = env_config.get("platform", "")
    if not platform:
        raise TaskGraphError("No 'platform' specified in environment config")

    # Normalize platform identifiers
    platform_lower = platform.lower().strip()

    # Handle URL-based platform specs (check before splitting by /)
    if platform_lower.startswith("http"):
        if "espressif32" in platform_lower:
            return "espressif32"
        elif "atmelavr" in platform_lower:
            return "atmelavr"
        elif "teensy" in platform_lower:
            return "teensy"
        elif "ststm32" in platform_lower:
            return "ststm32"
        elif "raspberrypi" in platform_lower:
            return "raspberrypi"

    # Handle platformio shorthand (e.g., "platformio/espressif32" -> "espressif32")
    if "/" in platform_lower:
        platform_lower = platform_lower.split("/")[-1]

    return platform_lower


def build_task_graph(
    project_path: Path,
    env_name: str,
    cache: Cache,
) -> list[PackageTask]:
    """Build a PackageTask dependency graph for any supported platform.

    Auto-detects the platform from platformio.ini and delegates to the
    appropriate platform-specific adapter function.

    Currently supports:
    - AVR (atmelavr): Arduino Uno, Mega, Nano, etc.

    Future support planned for:
    - ESP32 (espressif32)
    - Teensy
    - STM32

    Args:
        project_path: Path to the project directory containing platformio.ini.
        env_name: Environment name from platformio.ini (e.g. "uno").
        cache: Cache instance for determining destination paths.

    Returns:
        List of PackageTask instances forming the dependency graph.

    Raises:
        TaskGraphError: If the project config is invalid or platform is unsupported.
    """
    # Parse platformio.ini to detect platform
    ini_path = project_path / "platformio.ini"
    try:
        config = PlatformIOConfig(ini_path)
        env_config = config.get_env_config(env_name)
    except KeyboardInterrupt:
        raise
    except Exception as e:
        raise TaskGraphError(f"Failed to parse {ini_path}: {e}")

    platform = _detect_platform(env_config)

    if platform == "atmelavr":
        return build_avr_task_graph(project_path, env_name, cache)
    else:
        raise TaskGraphError(f"Platform '{platform}' is not yet supported by the parallel pipeline. Supported platforms: atmelavr")


def is_task_cached(task: PackageTask, cache: Cache) -> bool:
    """Check if a package task is already satisfied by the cache.

    Examines the destination path to determine if the package is already
    installed and valid. Cached tasks can be skipped by the pipeline.

    Args:
        task: PackageTask to check.
        cache: Cache instance for path resolution.

    Returns:
        True if the task's destination path exists and contains files.
    """
    dest = Path(task.dest_path)
    if not dest.exists() or not dest.is_dir():
        return False

    # Check that directory is not empty
    try:
        next(dest.iterdir())
        return True
    except StopIteration:
        return False


def filter_uncached_tasks(
    tasks: list[PackageTask],
    cache: Cache,
) -> tuple[list[PackageTask], list[PackageTask]]:
    """Separate tasks into cached (skip) and uncached (need processing) groups.

    Tasks that are already cached are returned in the first list with their
    phase set to DONE. Tasks that need processing are returned in the second
    list unchanged.

    Args:
        tasks: List of PackageTask instances to check.
        cache: Cache instance for path resolution.

    Returns:
        Tuple of (cached_tasks, uncached_tasks).
    """
    from .models import TaskPhase

    cached: list[PackageTask] = []
    uncached: list[PackageTask] = []

    for task in tasks:
        if is_task_cached(task, cache):
            task.phase = TaskPhase.DONE
            task.status_text = "Cached"
            cached.append(task)
        else:
            uncached.append(task)

    return cached, uncached
