"""Tests for build state tracking and cache invalidation."""



from fbuild.build.build_state import BuildState, BuildStateTracker


class TestBuildState:
    """Test BuildState class."""

    def test_to_dict_and_from_dict(self):
        """Test serialization and deserialization."""
        state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
            platform_version="1.8.6",
            build_flags=["-DDEBUG", "-O2"],
            lib_deps=["https://github.com/fastled/FastLED"],
        )

        # Convert to dict
        data = state.to_dict()

        # Convert back from dict
        restored = BuildState.from_dict(data)

        # Verify all fields match
        assert restored.platformio_ini_hash == state.platformio_ini_hash
        assert restored.platform == state.platform
        assert restored.board == state.board
        assert restored.framework == state.framework
        assert restored.toolchain_version == state.toolchain_version
        assert restored.framework_version == state.framework_version
        assert restored.platform_version == state.platform_version
        assert restored.build_flags == state.build_flags
        assert restored.lib_deps == state.lib_deps

    def test_save_and_load(self, tmp_path):
        """Test saving and loading from file."""
        state_file = tmp_path / "build_state.json"

        state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
        )

        # Save
        state.save(state_file)
        assert state_file.exists()

        # Load
        loaded = BuildState.load(state_file)
        assert loaded is not None
        assert loaded.platformio_ini_hash == "abc123"
        assert loaded.platform == "atmelavr"

    def test_load_nonexistent_file(self, tmp_path):
        """Test loading from nonexistent file returns None."""
        state_file = tmp_path / "nonexistent.json"
        loaded = BuildState.load(state_file)
        assert loaded is None

    def test_load_corrupted_file(self, tmp_path):
        """Test loading corrupted JSON returns None."""
        state_file = tmp_path / "corrupted.json"
        state_file.write_text("not valid json {{{")

        loaded = BuildState.load(state_file)
        assert loaded is None

    def test_compare_no_previous_state(self):
        """Test comparison with no previous state."""
        current = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )

        needs_rebuild, reasons = current.compare(None)

        assert needs_rebuild is True
        assert "No previous build state found" in reasons

    def test_compare_unchanged(self):
        """Test comparison with unchanged state."""
        state1 = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
        )

        state2 = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
        )

        needs_rebuild, reasons = state2.compare(state1)

        assert needs_rebuild is False
        assert len(reasons) == 0

    def test_compare_platformio_ini_changed(self):
        """Test detection of platformio.ini changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )

        new_state = BuildState(
            platformio_ini_hash="def456",  # Changed hash
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert "platformio.ini has changed" in reasons

    def test_compare_toolchain_version_changed(self):
        """Test detection of toolchain version changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
        )

        new_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="8.0.0",  # Changed version
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert any("Toolchain version changed" in r for r in reasons)

    def test_compare_framework_version_changed(self):
        """Test detection of framework version changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            framework_version="1.8.6",
        )

        new_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            framework_version="2.0.0",  # Changed version
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert any("Framework version changed" in r for r in reasons)

    def test_compare_build_flags_changed(self):
        """Test detection of build flag changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            build_flags=["-DDEBUG"],
        )

        new_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            build_flags=["-DRELEASE"],  # Changed flags
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert "Build flags have changed" in reasons

    def test_compare_lib_deps_changed(self):
        """Test detection of library dependency changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            lib_deps=["https://github.com/lib1"],
        )

        new_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            lib_deps=["https://github.com/lib2"],  # Changed deps
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert "Library dependencies have changed" in reasons

    def test_compare_multiple_changes(self):
        """Test detection of multiple changes."""
        old_state = BuildState(
            platformio_ini_hash="abc123",
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
        )

        new_state = BuildState(
            platformio_ini_hash="def456",  # Changed
            platform="atmelavr",
            board="mega",  # Changed
            framework="arduino",
            toolchain_version="8.0.0",  # Changed
            framework_version="1.8.6",
        )

        needs_rebuild, reasons = new_state.compare(old_state)

        assert needs_rebuild is True
        assert len(reasons) == 3  # Should have 3 reasons
        assert any("platformio.ini" in r for r in reasons)
        assert any("Board changed" in r for r in reasons)
        assert any("Toolchain version changed" in r for r in reasons)


class TestBuildStateTracker:
    """Test BuildStateTracker class."""

    def test_hash_file(self, tmp_path):
        """Test file hashing."""
        test_file = tmp_path / "test.txt"
        test_file.write_text("Hello, world!")

        hash1 = BuildStateTracker.hash_file(test_file)

        # Hash should be consistent
        hash2 = BuildStateTracker.hash_file(test_file)
        assert hash1 == hash2

        # Different content should produce different hash
        test_file.write_text("Different content")
        hash3 = BuildStateTracker.hash_file(test_file)
        assert hash1 != hash3

    def test_create_state(self, tmp_path):
        """Test creating a build state."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)
        state = tracker.create_state(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
            framework_version="1.8.6",
        )

        assert state.platform == "atmelavr"
        assert state.board == "uno"
        assert state.toolchain_version == "7.3.0"
        assert state.platformio_ini_hash is not None
        assert len(state.platformio_ini_hash) == 64  # SHA256 hex length

    def test_save_and_load_state(self, tmp_path):
        """Test saving and loading state through tracker."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)

        # Create and save state
        state = tracker.create_state(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )
        tracker.save_state(state)

        # Load state
        loaded = tracker.load_previous_state()
        assert loaded is not None
        assert loaded.platform == "atmelavr"
        assert loaded.board == "uno"

    def test_check_invalidation_first_build(self, tmp_path):
        """Test invalidation check with no previous build."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)

        needs_rebuild, reasons, current_state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )

        assert needs_rebuild is True
        assert "No previous build state found" in reasons
        assert current_state is not None

    def test_check_invalidation_unchanged(self, tmp_path):
        """Test invalidation check with unchanged configuration."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)

        # First build - save state
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
        )
        tracker.save_state(state)

        # Second build - should not need rebuild
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
        )

        assert needs_rebuild is False
        assert len(reasons) == 0

    def test_check_invalidation_ini_changed(self, tmp_path):
        """Test invalidation when platformio.ini changes."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)

        # First build
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )
        tracker.save_state(state)

        # Modify platformio.ini
        ini_file.write_text("[env:uno]\nplatform = atmelavr\nbuild_flags = -DDEBUG\n")

        # Second build - should need rebuild
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
        )

        assert needs_rebuild is True
        assert "platformio.ini has changed" in reasons

    def test_check_invalidation_version_changed(self, tmp_path):
        """Test invalidation when toolchain version changes."""
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        ini_file = tmp_path / "platformio.ini"
        ini_file.write_text("[env:uno]\nplatform = atmelavr\n")

        tracker = BuildStateTracker(build_dir)

        # First build with version 7.3.0
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="7.3.0",
        )
        tracker.save_state(state)

        # Second build with version 8.0.0
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=ini_file,
            platform="atmelavr",
            board="uno",
            framework="arduino",
            toolchain_version="8.0.0",
        )

        assert needs_rebuild is True
        assert any("Toolchain version changed" in r for r in reasons)
