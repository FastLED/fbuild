"""
Unit tests for platform_configs module.

Tests the platform configuration loader that provides MCU-specific
compiler/linker settings from packaged JSON files.
"""

from fbuild import platform_configs


class TestLoadConfig:
    """Tests for platform_configs.load_config()"""

    def test_load_esp32_config(self):
        """Load ESP32 config from esp/ subdirectory."""
        config = platform_configs.load_config("esp32")
        assert config is not None
        assert config["name"] == "ESP32"
        assert config["mcu"] == "esp32"
        assert "compiler_flags" in config
        assert "linker_flags" in config

    def test_load_esp32c6_config(self):
        """Load ESP32-C6 config from esp/ subdirectory."""
        config = platform_configs.load_config("esp32c6")
        assert config is not None
        assert config["mcu"] == "esp32c6"

    def test_load_esp32s3_config(self):
        """Load ESP32-S3 config from esp/ subdirectory."""
        config = platform_configs.load_config("esp32s3")
        assert config is not None
        assert config["mcu"] == "esp32s3"

    def test_load_rp2040_config(self):
        """Load RP2040 config from rp/ subdirectory."""
        config = platform_configs.load_config("rp2040")
        assert config is not None
        assert config["mcu"] == "rp2040"

    def test_load_rp2350_config(self):
        """Load RP2350 config from rp/ subdirectory."""
        config = platform_configs.load_config("rp2350")
        assert config is not None
        assert config["mcu"] == "rp2350"

    def test_load_stm32f1_config(self):
        """Load STM32F1 config from stm32/ subdirectory."""
        config = platform_configs.load_config("stm32f1")
        assert config is not None
        assert config["mcu"] == "stm32f1"

    def test_load_stm32f4_config(self):
        """Load STM32F4 config from stm32/ subdirectory."""
        config = platform_configs.load_config("stm32f4")
        assert config is not None
        assert config["mcu"] == "stm32f4"

    def test_load_imxrt1062_config(self):
        """Load imxrt1062 (Teensy 4.x) config from teensy/ subdirectory."""
        config = platform_configs.load_config("imxrt1062")
        assert config is not None
        assert config["mcu"] == "imxrt1062"

    def test_load_nonexistent_config_returns_none(self):
        """Loading a non-existent MCU config returns None."""
        config = platform_configs.load_config("nonexistent_mcu_12345")
        assert config is None

    def test_load_empty_string_returns_none(self):
        """Loading with empty string returns None."""
        config = platform_configs.load_config("")
        assert config is None

    def test_config_has_required_fields(self):
        """All configs should have required fields."""
        required_fields = ["name", "mcu", "compiler_flags"]

        for mcu in platform_configs.list_available_configs():
            config = platform_configs.load_config(mcu)
            assert config is not None, f"Failed to load config for {mcu}"
            for field in required_fields:
                assert field in config, f"Config for {mcu} missing required field: {field}"


class TestListAvailableConfigs:
    """Tests for platform_configs.list_available_configs()"""

    def test_returns_list(self):
        """Should return a list."""
        configs = platform_configs.list_available_configs()
        assert isinstance(configs, list)

    def test_contains_expected_mcus(self):
        """Should contain all expected MCU configs."""
        configs = platform_configs.list_available_configs()

        expected_mcus = [
            "esp32",
            "esp32c2",
            "esp32c3",
            "esp32c5",
            "esp32c6",
            "esp32p4",
            "esp32s3",
            "rp2040",
            "rp2350",
            "stm32f1",
            "stm32f4",
            "imxrt1062",
        ]

        for mcu in expected_mcus:
            assert mcu in configs, f"Expected MCU {mcu} not found in available configs"

    def test_list_is_sorted(self):
        """Returned list should be sorted alphabetically."""
        configs = platform_configs.list_available_configs()
        assert configs == sorted(configs)

    def test_no_json_extension(self):
        """Returned MCU names should not have .json extension."""
        configs = platform_configs.list_available_configs()
        for config in configs:
            assert not config.endswith(".json"), f"Config {config} has .json extension"


class TestListConfigsByVendor:
    """Tests for platform_configs.list_configs_by_vendor()"""

    def test_returns_dict(self):
        """Should return a dictionary."""
        by_vendor = platform_configs.list_configs_by_vendor()
        assert isinstance(by_vendor, dict)

    def test_contains_expected_vendors(self):
        """Should contain all expected vendor keys."""
        by_vendor = platform_configs.list_configs_by_vendor()
        expected_vendors = ["esp", "teensy", "rp", "stm32"]

        for vendor in expected_vendors:
            assert vendor in by_vendor, f"Expected vendor {vendor} not found"

    def test_esp_vendor_configs(self):
        """ESP vendor should contain all ESP32 variants."""
        by_vendor = platform_configs.list_configs_by_vendor()
        esp_configs = by_vendor.get("esp", [])

        expected = ["esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32p4", "esp32s3"]
        for mcu in expected:
            assert mcu in esp_configs, f"Expected {mcu} in esp vendor"

    def test_teensy_vendor_configs(self):
        """Teensy vendor should contain imxrt1062."""
        by_vendor = platform_configs.list_configs_by_vendor()
        teensy_configs = by_vendor.get("teensy", [])

        assert "imxrt1062" in teensy_configs

    def test_rp_vendor_configs(self):
        """RP vendor should contain RP2040 and RP2350."""
        by_vendor = platform_configs.list_configs_by_vendor()
        rp_configs = by_vendor.get("rp", [])

        assert "rp2040" in rp_configs
        assert "rp2350" in rp_configs

    def test_stm32_vendor_configs(self):
        """STM32 vendor should contain STM32F1 and STM32F4."""
        by_vendor = platform_configs.list_configs_by_vendor()
        stm32_configs = by_vendor.get("stm32", [])

        assert "stm32f1" in stm32_configs
        assert "stm32f4" in stm32_configs

    def test_vendor_lists_are_sorted(self):
        """MCU lists within each vendor should be sorted."""
        by_vendor = platform_configs.list_configs_by_vendor()

        for vendor, mcus in by_vendor.items():
            assert mcus == sorted(mcus), f"MCU list for vendor {vendor} is not sorted"


class TestVendorDirectoryStructure:
    """Tests to verify the vendor directory structure is correct."""

    def test_all_configs_loadable(self):
        """Every config in list_available_configs should be loadable."""
        for mcu in platform_configs.list_available_configs():
            config = platform_configs.load_config(mcu)
            assert config is not None, f"Config for {mcu} listed but not loadable"

    def test_vendor_configs_match_available(self):
        """Configs from list_configs_by_vendor should match list_available_configs."""
        available = set(platform_configs.list_available_configs())

        by_vendor = platform_configs.list_configs_by_vendor()
        from_vendors = set()
        for mcus in by_vendor.values():
            from_vendors.update(mcus)

        assert available == from_vendors, "Mismatch between available configs and vendor configs"

    def test_vendor_dirs_constant(self):
        """VENDOR_DIRS constant should exist and contain expected vendors."""
        assert hasattr(platform_configs, "VENDOR_DIRS")
        assert "esp" in platform_configs.VENDOR_DIRS
        assert "teensy" in platform_configs.VENDOR_DIRS
        assert "rp" in platform_configs.VENDOR_DIRS
        assert "stm32" in platform_configs.VENDOR_DIRS


class TestManifest:
    """Tests for manifest.json to ensure it stays in sync with actual config files."""

    def test_manifest_loads(self):
        """Manifest should load successfully."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None
        assert isinstance(manifest, dict)

    def test_manifest_has_required_fields(self):
        """Manifest should have required top-level fields."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        assert "version" in manifest
        assert "description" in manifest
        assert "vendors" in manifest

    def test_manifest_version_is_integer(self):
        """Manifest version should be an integer."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None
        assert isinstance(manifest["version"], int)

    def test_manifest_vendors_match_vendor_dirs(self):
        """Manifest vendors should match VENDOR_DIRS constant."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        manifest_vendors = set(manifest["vendors"].keys())
        code_vendors = set(platform_configs.VENDOR_DIRS)

        assert manifest_vendors == code_vendors, f"Manifest vendors {manifest_vendors} don't match VENDOR_DIRS {code_vendors}"

    def test_manifest_vendors_have_required_fields(self):
        """Each vendor in manifest should have required fields."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        required_fields = ["name", "description", "configs"]

        for vendor, vendor_data in manifest["vendors"].items():
            for field in required_fields:
                assert field in vendor_data, f"Vendor {vendor} missing required field: {field}"
            assert isinstance(vendor_data["configs"], list), f"Vendor {vendor} configs should be a list"

    def test_manifest_configs_match_actual_files(self):
        """Manifest configs should exactly match actual config files on disk.

        This is the critical test that ensures the manifest stays up to date.
        If you add or remove a config file, you must update manifest.json.
        """
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        # Get configs from manifest
        manifest_configs: dict[str, set[str]] = {}
        for vendor, vendor_data in manifest["vendors"].items():
            manifest_configs[vendor] = set(vendor_data["configs"])

        # Get actual configs from disk
        actual_configs = platform_configs.list_configs_by_vendor()
        actual_configs_sets: dict[str, set[str]] = {vendor: set(mcus) for vendor, mcus in actual_configs.items()}

        # Check each vendor
        for vendor in platform_configs.VENDOR_DIRS:
            manifest_set = manifest_configs.get(vendor, set())
            actual_set = actual_configs_sets.get(vendor, set())

            # Check for configs in manifest but not on disk
            missing_on_disk = manifest_set - actual_set
            assert not missing_on_disk, f"Vendor '{vendor}': manifest.json lists configs that don't exist on disk: {missing_on_disk}. " f"Remove them from manifest.json."

            # Check for configs on disk but not in manifest
            missing_in_manifest = actual_set - manifest_set
            assert not missing_in_manifest, f"Vendor '{vendor}': config files exist on disk but not in manifest.json: {missing_in_manifest}. " f"Add them to manifest.json."

    def test_manifest_all_configs_loadable(self):
        """Every config listed in manifest should be loadable."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        for vendor, vendor_data in manifest["vendors"].items():
            for mcu in vendor_data["configs"]:
                config = platform_configs.load_config(mcu)
                assert config is not None, f"Config '{mcu}' listed in manifest under vendor '{vendor}' but failed to load"

    def test_manifest_configs_are_sorted(self):
        """Config lists in manifest should be sorted for consistency."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        for vendor, vendor_data in manifest["vendors"].items():
            configs = vendor_data["configs"]
            assert configs == sorted(configs), f"Vendor '{vendor}' configs are not sorted in manifest.json. " f"Expected: {sorted(configs)}"

    def test_manifest_total_config_count(self):
        """Manifest should have same total config count as actual files."""
        manifest = platform_configs.load_manifest()
        assert manifest is not None

        manifest_count = sum(len(vendor_data["configs"]) for vendor_data in manifest["vendors"].values())

        actual_count = len(platform_configs.list_available_configs())

        assert manifest_count == actual_count, f"Manifest lists {manifest_count} configs but {actual_count} exist on disk"
