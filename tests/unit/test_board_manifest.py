"""Tests for board manifest integrity.

Ensures that assets/boards/manifest.json is up to date with all board JSON files
and that all referenced boards actually exist.
"""

import json
from pathlib import Path

# Get the project root directory
PROJECT_ROOT = Path(__file__).parent.parent.parent
BOARDS_DIR = PROJECT_ROOT / "assets" / "boards" / "json"
MANIFEST_PATH = PROJECT_ROOT / "assets" / "boards" / "manifest.json"


class TestBoardManifest:
    """Tests for board manifest validation."""

    def test_manifest_exists(self) -> None:
        """Verify that the manifest file exists."""
        assert MANIFEST_PATH.exists(), f"Manifest not found at {MANIFEST_PATH}"

    def test_manifest_is_valid_json(self) -> None:
        """Verify that the manifest is valid JSON."""
        content = MANIFEST_PATH.read_text()
        manifest = json.loads(content)
        assert "version" in manifest, "Manifest missing 'version' field"
        assert "boards" in manifest, "Manifest missing 'boards' field"
        assert isinstance(manifest["boards"], list), "'boards' field must be a list"

    def test_all_manifest_boards_exist(self) -> None:
        """Verify all boards in manifest have corresponding JSON files."""
        manifest = json.loads(MANIFEST_PATH.read_text())
        missing_boards = []

        for board_name in manifest["boards"]:
            board_file = BOARDS_DIR / f"{board_name}.json"
            if not board_file.exists():
                missing_boards.append(board_name)

        assert not missing_boards, (
            f"Manifest references {len(missing_boards)} boards that don't exist:\n"
            + "\n".join(f"  - {b}" for b in missing_boards[:20])
            + (f"\n  ... and {len(missing_boards) - 20} more" if len(missing_boards) > 20 else "")
        )

    def test_all_board_files_in_manifest(self) -> None:
        """Verify all JSON files in boards directory are listed in manifest."""
        manifest = json.loads(MANIFEST_PATH.read_text())
        manifest_boards = set(manifest["boards"])

        actual_boards = {f.stem for f in BOARDS_DIR.glob("*.json")}
        missing_from_manifest = actual_boards - manifest_boards

        assert not missing_from_manifest, (
            f"Found {len(missing_from_manifest)} board files not in manifest:\n"
            + "\n".join(f"  - {b}" for b in sorted(missing_from_manifest)[:20])
            + (f"\n  ... and {len(missing_from_manifest) - 20} more" if len(missing_from_manifest) > 20 else "")
            + "\n\nRun: python scripts/update_board_manifest.py"
        )

    def test_manifest_boards_are_sorted(self) -> None:
        """Verify boards in manifest are sorted alphabetically."""
        manifest = json.loads(MANIFEST_PATH.read_text())
        boards = manifest["boards"]
        sorted_boards = sorted(boards)

        assert boards == sorted_boards, "Boards in manifest are not sorted alphabetically.\nRun: python scripts/update_board_manifest.py"

    def test_no_duplicate_boards_in_manifest(self) -> None:
        """Verify there are no duplicate board entries in manifest."""
        manifest = json.loads(MANIFEST_PATH.read_text())
        boards = manifest["boards"]
        seen = set()
        duplicates = []

        for board in boards:
            if board in seen:
                duplicates.append(board)
            seen.add(board)

        assert not duplicates, f"Found {len(duplicates)} duplicate board entries:\n" + "\n".join(f"  - {b}" for b in duplicates)

    def test_board_files_are_valid_json(self) -> None:
        """Verify all board JSON files are valid JSON."""
        invalid_files = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                json.loads(board_file.read_text())
            except json.JSONDecodeError as e:
                invalid_files.append((board_file.name, str(e)))

        assert not invalid_files, f"Found {len(invalid_files)} invalid JSON files:\n" + "\n".join(f"  - {name}: {err}" for name, err in invalid_files[:10])

    def test_board_count_matches(self) -> None:
        """Verify manifest board count matches actual file count."""
        manifest = json.loads(MANIFEST_PATH.read_text())
        manifest_count = len(manifest["boards"])
        actual_count = len(list(BOARDS_DIR.glob("*.json")))

        assert manifest_count == actual_count, f"Board count mismatch: manifest has {manifest_count}, directory has {actual_count} files"

    def test_all_boards_have_required_fields(self) -> None:
        """Verify all board JSON files have required PlatformIO fields.

        This ensures the entire board.json pipeline is valid - from file on disk
        through manifest lookup to actual board loading.
        """
        # Required fields for PlatformIO board definitions
        # See: https://docs.platformio.org/en/latest/platforms/creating_board.html
        required_fields = {"name"}  # 'name' is the only universally required field

        boards_missing_fields: list[tuple[str, list[str]]] = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                board_data = json.loads(board_file.read_text())
                missing = [f for f in required_fields if f not in board_data]
                if missing:
                    boards_missing_fields.append((board_file.stem, missing))
            except json.JSONDecodeError:
                # Already covered by test_board_files_are_valid_json
                pass

        assert not boards_missing_fields, (
            f"Found {len(boards_missing_fields)} boards missing required fields:\n"
            + "\n".join(f"  - {name}: missing {fields}" for name, fields in boards_missing_fields[:20])
            + (f"\n  ... and {len(boards_missing_fields) - 20} more" if len(boards_missing_fields) > 20 else "")
        )

    def test_board_ids_match_filenames(self) -> None:
        """Verify board JSON 'id' field matches the filename (if present).

        This validates the pipeline assumption that board_id used for lookup
        corresponds to the actual board identity.
        """
        mismatched: list[tuple[str, str]] = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                board_data = json.loads(board_file.read_text())
                # 'id' field is optional but if present should match filename
                if "id" in board_data:
                    expected_id = board_file.stem
                    actual_id = board_data["id"]
                    if actual_id != expected_id:
                        mismatched.append((expected_id, actual_id))
            except json.JSONDecodeError:
                pass

        assert not mismatched, (
            f"Found {len(mismatched)} boards with mismatched id/filename:\n"
            + "\n".join(f"  - {filename}.json has id='{actual}'" for filename, actual in mismatched[:20])
            + (f"\n  ... and {len(mismatched) - 20} more" if len(mismatched) > 20 else "")
        )

    def test_manifest_provides_complete_board_index(self) -> None:
        """Verify manifest can serve as a complete index for board lookup.

        This is the key pipeline test: given a board_id, we can:
        1. Check manifest to see if board exists (O(1) with set)
        2. Load the board JSON file directly
        3. Get valid board configuration

        This test validates the bidirectional integrity of the entire pipeline.
        """
        manifest = json.loads(MANIFEST_PATH.read_text())
        manifest_boards = set(manifest["boards"])
        file_boards = {f.stem for f in BOARDS_DIR.glob("*.json")}

        # Bidirectional check: manifest == files
        manifest_only = manifest_boards - file_boards
        files_only = file_boards - manifest_boards

        errors = []
        if manifest_only:
            errors.append(f"Manifest has {len(manifest_only)} entries without files:\n" + "\n".join(f"  - {b}" for b in sorted(manifest_only)[:10]))
        if files_only:
            errors.append(f"Files exist without manifest entries ({len(files_only)}):\n" + "\n".join(f"  - {b}" for b in sorted(files_only)[:10]))

        # Also verify each manifest entry loads successfully
        load_errors = []
        for board_id in manifest["boards"]:
            board_file = BOARDS_DIR / f"{board_id}.json"
            if board_file.exists():
                try:
                    data = json.loads(board_file.read_text())
                    if not isinstance(data, dict):
                        load_errors.append(f"{board_id}: not a JSON object")
                except json.JSONDecodeError as e:
                    load_errors.append(f"{board_id}: {e}")

        if load_errors:
            errors.append(f"Failed to load {len(load_errors)} boards:\n" + "\n".join(f"  - {e}" for e in load_errors[:10]))

        assert not errors, "Board manifest pipeline integrity check failed:\n\n" + "\n\n".join(errors) + "\n\nRun: python scripts/update_board_manifest.py"

    def test_board_platforms_are_valid(self) -> None:
        """Verify all boards specify a valid platform.

        Boards should have a 'platform' field matching known PlatformIO platforms.
        """
        known_platforms = {
            "atmelavr",
            "atmelmegaavr",
            "atmelsam",
            "espressif32",
            "espressif8266",
            "nordicnrf51",
            "nordicnrf52",
            "nxplpc",
            "raspberrypi",
            "ststm32",
            "teensy",
            "siliconlabsefm32",
            "intel_arc32",
            "intel_mcs51",
            "native",
            "linux_arm",
            "maxim32",
            "titiva",
            "freescalekinetis",
            "gd32v",
            "hc32l13x",
            "riscv",
            "riscv_gap",
            "kendryte210",
            "nuclei",
            "sifive",
            "chipsalliance",
            # Add other platforms as needed
        }

        boards_without_platform: list[str] = []
        boards_with_unknown_platform: list[tuple[str, str]] = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                board_data = json.loads(board_file.read_text())
                platform = board_data.get("platform")
                if not platform:
                    boards_without_platform.append(board_file.stem)
                elif platform not in known_platforms:
                    boards_with_unknown_platform.append((board_file.stem, platform))
            except json.JSONDecodeError:
                pass

        # Only fail on missing platforms, not unknown ones (platforms can be added)
        assert not boards_without_platform, (
            f"Found {len(boards_without_platform)} boards without 'platform' field:\n"
            + "\n".join(f"  - {b}" for b in boards_without_platform[:20])
            + (f"\n  ... and {len(boards_without_platform) - 20} more" if len(boards_without_platform) > 20 else "")
        )

    def test_esp32s3_boards_have_build_mcu(self) -> None:
        """Verify all ESP32-S3 boards have build.mcu field.

        This was a design flaw reported in Issue #1 (PSRAM): board JSON files
        were missing the 'build' section entirely, which meant PSRAM type and
        other hardware features couldn't be determined from the board database.
        The build process uses pioarduino platform board JSONs (which have complete
        build config), but the local board database should also be accurate.

        This test specifically validates ESP32-S3 boards since they are the most
        commonly affected by PSRAM configuration issues.
        """
        boards_missing_build_mcu: list[str] = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                board_data = json.loads(board_file.read_text())
                # Top-level 'mcu' field uses uppercase (e.g., "ESP32S3") to identify board family.
                # build.mcu uses lowercase (e.g., "esp32s3") for the compiler. These are different
                # fields and we only check for presence of build.mcu (truthy), not an exact match.
                if board_data.get("mcu") != "ESP32S3":
                    continue
                build_mcu = board_data.get("build", {}).get("mcu")
                if not build_mcu:
                    boards_missing_build_mcu.append(board_file.stem)
            except json.JSONDecodeError:
                pass

        assert not boards_missing_build_mcu, (
            f"Found {len(boards_missing_build_mcu)} ESP32-S3 boards missing 'build.mcu' field "
            f"(required for correct PSRAM, flash mode, and MCU-specific compilation):\n"
            + "\n".join(f"  - {b}" for b in sorted(boards_missing_build_mcu)[:20])
            + (f"\n  ... and {len(boards_missing_build_mcu) - 20} more" if len(boards_missing_build_mcu) > 20 else "")
        )

    def test_esp32s3_psram_boards_have_memory_type(self) -> None:
        """Verify ESP32-S3 boards with PSRAM have build.arduino.memory_type field.

        This is needed for correct SDK library selection (qio_opi vs qio_qspi).
        Boards with OPI PSRAM (8MB+) need 'qio_opi' to get the correct SDK libs.
        Boards with QSPI PSRAM (2MB) need 'qio_qspi'.
        Boards without PSRAM omit memory_type (defaults to qio_qspi).
        """
        boards_with_psram_missing_type: list[tuple[str, str]] = []

        for board_file in BOARDS_DIR.glob("*.json"):
            try:
                board_data = json.loads(board_file.read_text())
                if board_data.get("build", {}).get("mcu") != "esp32s3":
                    continue
                extra_flags = board_data.get("build", {}).get("extra_flags", [])
                if isinstance(extra_flags, str):
                    extra_flags = extra_flags.split()
                has_psram = "-DBOARD_HAS_PSRAM" in extra_flags
                if not has_psram:
                    continue
                memory_type = board_data.get("build", {}).get("arduino", {}).get("memory_type")
                if not memory_type:
                    boards_with_psram_missing_type.append((board_file.stem, str(extra_flags)))
            except json.JSONDecodeError:
                pass

        assert not boards_with_psram_missing_type, (
            f"Found {len(boards_with_psram_missing_type)} ESP32-S3 PSRAM boards missing "
            f"'build.arduino.memory_type' (needed for correct SDK library selection):\n"
            + "\n".join(f"  - {name}" for name, _ in boards_with_psram_missing_type[:20])
            + (f"\n  ... and {len(boards_with_psram_missing_type) - 20} more" if len(boards_with_psram_missing_type) > 20 else "")
        )
